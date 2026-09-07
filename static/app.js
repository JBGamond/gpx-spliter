import init, { GpxAnalyzer, suggest_manual_paces } from './pkg/gpx_splitter.js';

let analyzer = null;
let routePoints = []; // full-resolution {lat, lon, elevation, distance_km}[] for the current route
let lastChunks = []; // chunks from the most recent analyze(), for hover lookups
let map = null;
let mapLoaded = false;
let hoverMarker = null;
let chartLayout = null; // { totalKm, padLeft, padRight, width, height } from the last chart draw, for hover math
let aidStations = []; // {id, name, lat, lon, elevation, distance_km, source: 'gpx'|'manual'}[]
let aidMarkers = []; // maplibregl.Marker instances mirroring aidStations, in the same order

const status = document.getElementById('status');
const resultsContainer = document.getElementById('results-container');
const downloadBtn = document.getElementById('download-btn');
const previewSection = document.getElementById('preview-section');
const chartTooltip = document.getElementById('chart-tooltip');

async function start() {
    try {
        await init();
        showStatus('WASM Engine loaded.', 'info');
        setTimeout(hideStatus, 2000);
    } catch (e) {
        showStatus('Failed to load WASM engine: ' + e, 'error');
    }
}

const showStatus = (msg, type) => {
    status.style.display = 'block';
    status.className = type;
    status.textContent = msg;
};

const hideStatus = () => {
    status.style.display = 'none';
};

const parsePace = (str) => {
    const parts = str.split(':');
    if (parts.length !== 2) return 300 / 1000; // Default 5:00
    const mins = parseInt(parts[0]);
    const secs = parseInt(parts[1]);
    return (mins * 60 + secs) / 1000; // sec/m
};

// Inverse of parsePace: sec/m -> "MM:SS" for populating pace inputs.
const formatPaceInput = (secPerM) => {
    const totalSec = Math.round(secPerM * 1000);
    const m = Math.floor(totalSec / 60);
    const s = totalSec % 60;
    return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
};

const getManualPaces = () => {
    return {
        flat: parsePace(document.getElementById('p-flat').value),
        gentle_uphill: parsePace(document.getElementById('p-gentle-up').value),
        moderate_uphill: parsePace(document.getElementById('p-mod-up').value),
        steep_uphill: parsePace(document.getElementById('p-steep-up').value),
        very_steep_uphill: parsePace(document.getElementById('p-vsteep-up').value),
        gentle_downhill: parsePace(document.getElementById('p-gentle-down').value),
        moderate_downhill: parsePace(document.getElementById('p-mod-down').value),
        steep_downhill: parsePace(document.getElementById('p-steep-down').value),
        very_steep_downhill: parsePace(document.getElementById('p-vsteep-down').value),
    };
};

const getGradientColor = (grad) => {
    if (grad > 0) {
        const alpha = Math.min(grad / 20, 0.8);
        return `rgba(231, 76, 60, ${alpha})`;
    } else if (grad < 0) {
        const alpha = Math.min(Math.abs(grad) / 20, 0.8);
        return `rgba(52, 152, 219, ${alpha})`;
    }
    return 'transparent';
};

// ---- Route preview (map + elevation chart) -------------------------------
//
// Both the map line and the elevation chart are colored with the same scale:
// by estimated pace (fast = green, slow = red) whenever pace data is
// available (reference activity or manual paces), falling back to a
// gradient-based scale (downhill = blue, uphill = red) before that data
// exists.

const PACE_COLOR_STOPS = ['#2ecc71', '#f1c40f', '#e74c3c']; // fast -> mid -> slow
const GRADIENT_COLOR_STOPS = ['#3498db', '#bdc3c7', '#e74c3c']; // downhill -> flat -> uphill

const hexToRgb = (hex) => {
    const n = parseInt(hex.slice(1), 16);
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
};

const lerpColor = (hexA, hexB, t) => {
    const a = hexToRgb(hexA);
    const b = hexToRgb(hexB);
    const r = Math.round(a.r + (b.r - a.r) * t);
    const g = Math.round(a.g + (b.g - a.g) * t);
    const bl = Math.round(a.b + (b.b - a.b) * t);
    return `rgb(${r}, ${g}, ${bl})`;
};

const colorFromStops = (stops, t) => {
    t = Math.max(0, Math.min(1, t));
    const n = stops.length - 1;
    const seg = t * n;
    const i = Math.min(Math.floor(seg), n - 1);
    return lerpColor(stops[i], stops[i + 1], seg - i);
};

const paceToSeconds = (paceStr) => {
    const [m, s] = paceStr.split(':').map(Number);
    return m * 60 + s;
};

// Build a { legend, colorForChunk } scale from the current chunk list.
const buildColorScale = (chunks) => {
    const hasPace = chunks.some(c => c.estimated_pace);

    if (hasPace) {
        const paces = chunks.filter(c => c.estimated_pace).map(c => paceToSeconds(c.estimated_pace));
        const min = Math.min(...paces);
        const max = Math.max(...paces);
        return {
            legend: { from: 'Fast', to: 'Slow', stops: PACE_COLOR_STOPS },
            colorForChunk: (c) => {
                if (!c.estimated_pace) return '#999999';
                const t = max > min ? (paceToSeconds(c.estimated_pace) - min) / (max - min) : 0;
                return colorFromStops(PACE_COLOR_STOPS, t);
            },
        };
    }

    const maxAbsGrad = Math.max(10, ...chunks.map(c => Math.abs(c.gradient_pct)));
    return {
        legend: { from: 'Downhill', to: 'Uphill', stops: GRADIENT_COLOR_STOPS },
        colorForChunk: (c) => colorFromStops(GRADIENT_COLOR_STOPS, (c.gradient_pct / maxAbsGrad + 1) / 2),
    };
};

// Find the chunk covering a given cumulative distance (km).
const chunkAtDistance = (chunks, km) => {
    for (const c of chunks) {
        if (km <= c.cumulative_km + 1e-9) return c;
    }
    return chunks[chunks.length - 1];
};

const updateLegend = (legend) => {
    const el = document.getElementById('chart-legend');
    if (!el) return;
    el.innerHTML = `
        <span>${legend.from}</span>
        <div class="legend-bar" style="background: linear-gradient(to right, ${legend.stops.join(', ')});"></div>
        <span>${legend.to}</span>
    `;
};

// MapLibre GL is loaded from a CDN in index.html. If that fails (offline,
// blocked domain, ad-blocker) `maplibregl` is undefined — the map/route
// preview should be skipped, not take down the rest of the app (grid, CSV
// export) with an uncaught ReferenceError.
const mapLibAvailable = () => typeof maplibregl !== 'undefined';

const ensureMap = () => {
    if (map || !mapLibAvailable()) return;
    map = new maplibregl.Map({
        container: 'map',
        style: {
            version: 8,
            sources: {
                osm: {
                    type: 'raster',
                    tiles: ['https://tile.openstreetmap.org/{z}/{x}/{y}.png'],
                    tileSize: 256,
                    attribution: '&copy; OpenStreetMap contributors',
                },
            },
            layers: [{ id: 'osm', type: 'raster', source: 'osm' }],
        },
        center: [0, 0],
        zoom: 1,
    });
    map.addControl(new maplibregl.NavigationControl({ showCompass: false }), 'top-right');
    map.on('load', () => { mapLoaded = true; });
};

const loadRouteIntoMap = (points) => {
    if (!points.length || !mapLibAvailable()) return;
    ensureMap();

    const geojson = {
        type: 'Feature',
        properties: {},
        geometry: { type: 'LineString', coordinates: points.map(p => [p.lon, p.lat]) },
    };

    const apply = () => {
        const source = map.getSource('route');
        if (source) {
            source.setData(geojson);
        } else {
            map.addSource('route', { type: 'geojson', lineMetrics: true, data: geojson });
            map.addLayer({
                id: 'route-line',
                type: 'line',
                source: 'route',
                paint: { 'line-width': 4, 'line-color': '#3498db' },
            });
        }

        const start = [points[0].lon, points[0].lat];
        const bounds = points.reduce((b, p) => b.extend([p.lon, p.lat]), new maplibregl.LngLatBounds(start, start));
        map.fitBounds(bounds, { padding: 30, duration: 0 });
    };

    if (mapLoaded) apply(); else map.on('load', apply);
};

// Build a MapLibre `line-gradient` expression that paints each chunk of the
// route in a flat color along its length, using ['line-progress'] (0..1).
//
// `interpolate` requires strictly ascending input values, so a hard step
// between two chunks' colors needs its two boundary stops pushed apart by a
// small epsilon rather than sharing the same x (which is what a naive
// "end of chunk i == start of chunk i+1" reuse produces, and what MapLibre's
// style validator now rejects outright).
const buildLineGradientExpression = (chunks, colorScale, totalKm) => {
    if (chunks.length === 0) throw new Error('no chunks to build a gradient from');
    const EPS = 1e-5;
    const stops = [];
    let cursor = 0;
    chunks.forEach((c, i) => {
        const color = colorScale.colorForChunk(c);
        const isLast = i === chunks.length - 1;
        let end = totalKm > 0 ? c.cumulative_km / totalKm : 1;
        end = isLast ? 1 : Math.min(1 - EPS, end);
        end = Math.max(end, cursor + EPS);
        stops.push([cursor, color]);
        stops.push([end, color]);
        cursor = isLast ? end : end + EPS;
    });
    stops[stops.length - 1][0] = 1; // guarantee the domain ends exactly at 1

    const expr = ['interpolate', ['linear'], ['line-progress']];
    stops.forEach(([ratio, color]) => expr.push(ratio, color));
    return expr;
};

const recolorRoute = (chunks) => {
    if (!map || !mapLoaded || !map.getLayer('route-line') || !chunks.length) return;
    const colorScale = buildColorScale(chunks);
    const totalKm = routePoints[routePoints.length - 1].distance_km;
    try {
        map.setPaintProperty('route-line', 'line-gradient', buildLineGradientExpression(chunks, colorScale, totalKm));
    } catch (e) {
        console.warn('line-gradient expression failed, falling back to a solid line', e);
        map.setPaintProperty('route-line', 'line-color', '#3498db');
    }
};

// ---- Aid stations (ravitaillements) ---------------------------------------
//
// Aid stations are points of interest snapped onto the route's cumulative
// distance, shown as flag markers on the map and as vertical markers on the
// elevation chart. They come from two sources: the GPX file's own <wpt>
// waypoints (loaded automatically per file) and manual entries added via the
// sidebar form.

const escapeHtml = (str) => str.replace(/[&<>"']/g, (c) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
}[c]));

const clearAidMarkers = () => {
    aidMarkers.forEach(m => m.remove());
    aidMarkers = [];
};

const renderAidMarkers = () => {
    clearAidMarkers();
    if (!map || !mapLibAvailable()) return;
    aidStations.forEach(station => {
        const el = document.createElement('div');
        el.className = 'aid-station-marker';
        el.title = station.name;
        const popup = new maplibregl.Popup({ offset: 20, closeButton: false }).setHTML(
            `<strong>${escapeHtml(station.name)}</strong><br>${station.distance_km.toFixed(2)} km &middot; ${station.elevation.toFixed(0)} m`
        );
        const marker = new maplibregl.Marker({ element: el, anchor: 'bottom' })
            .setLngLat([station.lon, station.lat])
            .setPopup(popup)
            .addTo(map);
        el.addEventListener('click', () => marker.togglePopup());
        aidMarkers.push(marker);
    });
};

const renderAidStationList = () => {
    const list = document.getElementById('aid-station-list');
    if (!list) return;
    if (!aidStations.length) {
        list.innerHTML = '<li class="aid-station-empty">No aid stations yet</li>';
        return;
    }
    list.innerHTML = aidStations.map(s => `
        <li data-id="${s.id}">
            <span class="aid-station-info">
                <strong>${escapeHtml(s.name)}</strong>
                <span class="aid-station-dist">${s.distance_km.toFixed(2)} km &middot; ${s.elevation.toFixed(0)} m</span>
            </span>
            <button type="button" class="aid-remove-btn" data-id="${s.id}" title="Remove">&times;</button>
        </li>
    `).join('');
};

// Re-renders every aid-station view: the sidebar list, the map markers, and
// (if a chart is currently on screen) its vertical markers.
const renderAidStations = () => {
    renderAidStationList();
    renderAidMarkers();
    if (routePoints.length && lastChunks.length) drawElevationChart(routePoints, lastChunks);
};

const removeAidStation = (id) => {
    aidStations = aidStations.filter(s => s.id !== id);
    renderAidStations();
};

const addAidStation = (name, km) => {
    const point = routePointAtDistance(km);
    if (!point) return;
    aidStations.push({
        id: `manual-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        name,
        lat: point.lat,
        lon: point.lon,
        elevation: point.elevation,
        distance_km: point.distance_km,
        source: 'manual',
    });
    aidStations.sort((a, b) => a.distance_km - b.distance_km);
    renderAidStations();
};

// Aid station markers on the elevation chart: a dashed vertical line plus a
// small dot, positioned with the same xScale/pad/height the profile line was
// just drawn with.
const drawAidStationMarkers = (ctx, xScale, top, bottom, totalKm) => {
    if (!aidStations.length) return;
    ctx.save();
    aidStations.forEach(station => {
        if (station.distance_km < 0 || station.distance_km > totalKm) return;
        const x = xScale(station.distance_km);
        ctx.strokeStyle = 'rgba(230, 126, 34, 0.85)';
        ctx.lineWidth = 1.5;
        ctx.setLineDash([4, 3]);
        ctx.beginPath();
        ctx.moveTo(x, top);
        ctx.lineTo(x, bottom);
        ctx.stroke();
        ctx.setLineDash([]);
        ctx.fillStyle = '#e67e22';
        ctx.beginPath();
        ctx.arc(x, top + 3, 4, 0, Math.PI * 2);
        ctx.fill();
    });
    ctx.restore();
};

// Horizontal elevation gridlines, every 100m by default. Very hilly routes
// (large min/max elevation spread) would turn 100m increments into an
// illegible ladder of lines, so the step widens to keep at most ~12 lines.
const ELEVATION_GRID_STEPS = [100, 200, 250, 500, 1000, 2000];
const pickElevationStep = (range) => {
    for (const step of ELEVATION_GRID_STEPS) {
        if (range <= 0 || range / step <= 12) return step;
    }
    return ELEVATION_GRID_STEPS[ELEVATION_GRID_STEPS.length - 1];
};

const drawElevationGrid = (ctx, yScale, minEle, maxEle, width, padLeft, padRight) => {
    const step = pickElevationStep(maxEle - minEle);
    const first = Math.ceil(minEle / step) * step;

    ctx.save();
    ctx.font = '10px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif';
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'right';
    ctx.lineWidth = 1;
    for (let ele = first; ele <= maxEle + 1e-6; ele += step) {
        const y = yScale(ele);
        ctx.strokeStyle = 'rgba(44, 62, 80, 0.1)';
        ctx.beginPath();
        ctx.moveTo(padLeft, y);
        ctx.lineTo(width - padRight, y);
        ctx.stroke();

        ctx.fillStyle = '#8a97a3';
        ctx.fillText(`${Math.round(ele)} m`, padLeft - 6, y);
    }
    ctx.restore();
};

// Vertical distance gridlines with "N km" labels along the bottom axis. Step
// widens on long routes the same way the elevation step does, to keep at
// most ~12 gridlines regardless of total distance.
const DISTANCE_GRID_STEPS_KM = [1, 2, 5, 10, 20, 25, 50, 100, 200];
const pickDistanceStep = (totalKm) => {
    for (const step of DISTANCE_GRID_STEPS_KM) {
        if (totalKm <= 0 || totalKm / step <= 12) return step;
    }
    return DISTANCE_GRID_STEPS_KM[DISTANCE_GRID_STEPS_KM.length - 1];
};

const drawDistanceGrid = (ctx, xScale, totalKm, width, height, padLeft, padRight, padTop, padBottom) => {
    const step = pickDistanceStep(totalKm);
    const bottom = height - padBottom;

    ctx.save();
    ctx.font = '10px -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif';
    ctx.textBaseline = 'top';
    ctx.lineWidth = 1;
    for (let km = 0; km <= totalKm + 1e-6; km += step) {
        const x = xScale(km);
        ctx.strokeStyle = 'rgba(44, 62, 80, 0.1)';
        ctx.beginPath();
        ctx.moveTo(x, padTop);
        ctx.lineTo(x, bottom);
        ctx.stroke();

        // Keep the first/last labels from spilling off the canvas edge.
        ctx.textAlign = x - padLeft < 12 ? 'left' : (width - padRight) - x < 12 ? 'right' : 'center';
        ctx.fillStyle = '#8a97a3';
        ctx.fillText(`${Math.round(km)} km`, x, bottom + 4);
    }
    ctx.restore();
};

const drawElevationChart = (points, chunks) => {
    const canvas = document.getElementById('elevation-chart');
    if (!canvas || points.length < 2 || chunks.length === 0) return;

    const ctx = canvas.getContext('2d');
    const dpr = window.devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    canvas.width = rect.width * dpr;
    canvas.height = rect.height * dpr;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

    const width = rect.width;
    const height = rect.height;
    ctx.clearRect(0, 0, width, height);

    const colorScale = buildColorScale(chunks);
    updateLegend(colorScale.legend);

    const totalKm = points[points.length - 1].distance_km;
    const elevations = points.map(p => p.elevation);
    const minEle = Math.min(...elevations);
    const maxEle = Math.max(...elevations);
    const padTop = 8;
    // padBottom makes room for the "N km" distance-axis labels below the
    // curve; padLeft makes room for the "1234 m" elevation labels. The
    // hover/click math (chartLayout) has to use the same horizontal offsets
    // as xScale below, or the two would disagree on where "km 0" actually is.
    const padLeft = 38;
    const padRight = 8;
    const padBottom = 20;
    chartLayout = { totalKm, padLeft, padRight, width, height };
    const xScale = (km) => (totalKm > 0 ? km / totalKm : 0) * (width - padLeft - padRight) + padLeft;
    const yScale = (ele) => {
        if (maxEle === minEle) return (height - padTop - padBottom) / 2 + padTop;
        return height - padBottom - ((ele - minEle) / (maxEle - minEle)) * (height - padTop - padBottom);
    };

    drawElevationGrid(ctx, yScale, minEle, maxEle, width, padLeft, padRight);
    drawDistanceGrid(ctx, xScale, totalKm, width, height, padLeft, padRight, padTop, padBottom);

    // Light fill under the curve for readability.
    ctx.beginPath();
    ctx.moveTo(xScale(points[0].distance_km), height - padBottom);
    points.forEach(p => ctx.lineTo(xScale(p.distance_km), yScale(p.elevation)));
    ctx.lineTo(xScale(points[points.length - 1].distance_km), height - padBottom);
    ctx.closePath();
    ctx.fillStyle = 'rgba(52, 73, 94, 0.06)';
    ctx.fill();

    // Colored profile line, one stroked segment per point-to-point step.
    ctx.lineWidth = 2.5;
    ctx.lineJoin = 'round';
    for (let i = 1; i < points.length; i++) {
        const p0 = points[i - 1];
        const p1 = points[i];
        const chunk = chunkAtDistance(chunks, (p0.distance_km + p1.distance_km) / 2);
        ctx.strokeStyle = colorScale.colorForChunk(chunk);
        ctx.beginPath();
        ctx.moveTo(xScale(p0.distance_km), yScale(p0.elevation));
        ctx.lineTo(xScale(p1.distance_km), yScale(p1.elevation));
        ctx.stroke();
    }

    drawAidStationMarkers(ctx, xScale, padTop, height - padBottom, totalKm);
};



const updatePreview = (chunks) => {
    if (!routePoints.length || !chunks || !chunks.length) return;
    lastChunks = chunks;
    recolorRoute(chunks);
    drawElevationChart(routePoints, chunks);
};

// ---- Chart <-> map hover sync ---------------------------------------------
//
// Hovering the elevation chart shows a tooltip with the target pace (or
// gradient, if no pace data is available yet) at that point, and drops a
// marker on the map at the matching location so it's clear where on the
// course that point of the chart actually is.

// Binary-search routePoints (sorted by distance_km) for the point closest
// to a given cumulative distance.
const routePointAtDistance = (km) => {
    if (!routePoints.length) return null;
    let lo = 0;
    let hi = routePoints.length - 1;
    while (lo < hi) {
        const mid = (lo + hi) >> 1;
        if (routePoints[mid].distance_km < km) lo = mid + 1; else hi = mid;
    }
    return routePoints[lo];
};

// Find an aid station close enough to the hovered km to be "at" this point
// of the chart, so the tooltip can call it out by name.
const aidStationNearDistance = (km, totalKm) => {
    const threshold = Math.max(totalKm * 0.004, 0.05);
    return aidStations.find(s => Math.abs(s.distance_km - km) <= threshold) || null;
};

const buildTooltipHtml = (point, chunk) => {
    const lines = [`<strong>${point.distance_km.toFixed(2)} km</strong> &middot; ${point.elevation.toFixed(0)} m`];
    const nearStation = chartLayout ? aidStationNearDistance(point.distance_km, chartLayout.totalKm) : null;
    if (nearStation) {
        lines.unshift(`🚩 <strong>${escapeHtml(nearStation.name)}</strong>`);
    }
    const sign = chunk.gradient_pct >= 0 ? '+' : '';
    lines.push(`${sign}${chunk.gradient_pct.toFixed(1)}% ${chunk.classification}`);
    if (chunk.estimated_pace) {
        lines.push(`Target pace: <strong>${chunk.estimated_pace} /km</strong>`);
    }
    if (chunk.cumulative_time) {
        lines.push(`At: ${chunk.cumulative_time}`);
    }
    return lines.join('<br>');
};

const showHoverMarker = (point) => {
    if (!map || !mapLoaded || !mapLibAvailable()) return;
    if (!hoverMarker) {
        const el = document.createElement('div');
        el.className = 'route-hover-marker';
        hoverMarker = new maplibregl.Marker({ element: el }).setLngLat([point.lon, point.lat]).addTo(map);
    } else {
        hoverMarker.setLngLat([point.lon, point.lat]);
    }
};

const hideHoverMarker = () => {
    if (hoverMarker) {
        hoverMarker.remove();
        hoverMarker = null;
    }
};

const onChartHover = (evt) => {
    if (!chartLayout || !routePoints.length || !lastChunks.length) return;

    const canvas = document.getElementById('elevation-chart');
    const rect = canvas.getBoundingClientRect();
    const x = evt.clientX - rect.left;
    const { totalKm, padLeft, padRight, width } = chartLayout;
    const t = (x - padLeft) / (width - padLeft - padRight);
    const km = Math.max(0, Math.min(totalKm, t * totalKm));

    const point = routePointAtDistance(km);
    const chunk = chunkAtDistance(lastChunks, km);
    if (!point || !chunk) return;

    chartTooltip.innerHTML = buildTooltipHtml(point, chunk);
    chartTooltip.style.left = `${Math.max(0, Math.min(width, x))}px`;
    chartTooltip.classList.remove('hidden');

    showHoverMarker(point);
};

const onChartHoverEnd = () => {
    chartTooltip.classList.add('hidden');
    hideHoverMarker();
};

document.getElementById('elevation-chart').addEventListener('mousemove', onChartHover);
document.getElementById('elevation-chart').addEventListener('mouseleave', onChartHoverEnd);

// Clicking the chart fills the aid-station distance field, so a station can
// be added at that point without needing to know its exact km beforehand.
document.getElementById('elevation-chart').addEventListener('click', (evt) => {
    if (!chartLayout) return;
    const canvas = document.getElementById('elevation-chart');
    const rect = canvas.getBoundingClientRect();
    const x = evt.clientX - rect.left;
    const { totalKm, padLeft, padRight, width } = chartLayout;
    const t = (x - padLeft) / (width - padLeft - padRight);
    const km = Math.max(0, Math.min(totalKm, t * totalKm));
    document.getElementById('aid-distance').value = km.toFixed(2);
    document.getElementById('aid-name').focus();
});

const updateGrid = (chunks) => {
    if (!chunks || chunks.length === 0) {
        resultsContainer.innerHTML = '<div class="empty-state">No chunks generated</div>';
        return;
    }

    let html = '<table><thead><tr>';
    html += '<th>#</th>';
    html += '<th>Dist (km)</th>';
    html += '<th>Cumul (km)</th>';
    html += '<th>D+ (m)</th>';
    html += '<th>D- (m)</th>';
    html += '<th>Cumul D+ (m)</th>';
    html += '<th>Cumul D- (m)</th>';
    html += '<th>Grad %</th>';
    html += '<th>Fatigue</th>';
    html += '<th>Class</th>';
    
    const hasPace = chunks[0].estimated_pace !== undefined;
    if (hasPace) {
        html += '<th>Pace</th>';
        html += '<th>Time</th>';
        html += '<th>Cumul Time</th>';
    }
    
    html += '</tr></thead><tbody>';

    chunks.forEach(c => {
        const gradColor = getGradientColor(c.gradient_pct);
        const textColor = Math.abs(c.gradient_pct) > 10 ? 'white' : 'inherit';

        html += `<tr>
            <td class="num">${c.chunk_number}</td>
            <td class="num">${c.distance_km.toFixed(3)}</td>
            <td class="num" style="font-weight: bold;">${c.cumulative_km.toFixed(2)}</td>
            <td class="num">${c.elevation_gain_m.toFixed(0)}</td>
            <td class="num">${c.elevation_loss_m.toFixed(0)}</td>
            <td class="num">${c.cumulative_gain_m.toFixed(0)}</td>
            <td class="num">${c.cumulative_loss_m.toFixed(0)}</td>
            <td class="num" style="background-color: ${gradColor}; color: ${textColor}; font-weight: bold;">${c.gradient_pct.toFixed(2)}%</td>
            <td class="num" style="color: #e67e22;">+${c.fatigue_pct.toFixed(1)}%</td>
            <td>${c.classification}</td>
        `;
        if (hasPace) {
            html += `
                <td class="num">${c.estimated_pace || '-'}</td>
                <td class="num">${c.estimated_time || '-'}</td>
                <td class="num">${c.cumulative_time || '-'}</td>
            `;
        }
        html += '</tr>';
    });

    html += '</tbody></table>';
    resultsContainer.innerHTML = html;
};

const readFile = (file) => {
    return new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(new Uint8Array(reader.result));
        reader.onerror = reject;
        reader.readAsArrayBuffer(file);
    });
};

const initAnalyzer = async () => {
    const inputFile = document.getElementById('input-file').files[0];

    if (!inputFile) {
        analyzer = null;
        routePoints = [];
        lastChunks = [];
        chartLayout = null;
        onChartHoverEnd();
        previewSection.classList.add('hidden');
        resultsContainer.innerHTML = '<div class="empty-state">Upload a GPX file to see the analysis grid</div>';
        aidStations = [];
        renderAidStationList();
        clearAidMarkers();
        return;
    }

    try {
        showStatus('Parsing files...', 'info');
        const inputData = await readFile(inputFile);

        analyzer = new GpxAnalyzer(inputData);

        // The route preview is a nice-to-have: if it fails (e.g. the MapLibre
        // CDN script didn't load) it must not take down file parsing/analysis.
        try {
            routePoints = analyzer.get_route_points();
            previewSection.classList.remove('hidden');
            loadRouteIntoMap(routePoints);

            // Aid stations embedded in the GPX file's own <wpt> waypoints,
            // if any. Manual entries from a previous file don't carry over.
            aidStations = [];
            try {
                aidStations = analyzer.get_aid_stations().map((s, i) => ({ ...s, id: `gpx-${i}`, source: 'gpx' }));
                aidStations.sort((a, b) => a.distance_km - b.distance_km);
            } catch (waypointError) {
                console.warn('Aid station lookup failed:', waypointError);
            }
            renderAidStations();
        } catch (previewError) {
            console.warn('Route preview unavailable:', previewError);
            routePoints = [];
            chartLayout = null;
            onChartHoverEnd();
            previewSection.classList.add('hidden');
            aidStations = [];
            renderAidStationList();
            clearAidMarkers();
        }

        hideStatus();
        runAnalysis();
    } catch (e) {
        showStatus('Error parsing files: ' + e, 'error');
        analyzer = null;
    }
};

// Shared by runAnalysis/downloadCSV: everything the manual pace/degradation
// model needs, read straight from the sidebar sliders.
const getManualModelInputs = () => ({
    tolerance: parseFloat(document.getElementById('tolerance').value),
    minDistance: parseFloat(document.getElementById('min-distance').value),
    manualPaces: getManualPaces(),
    degradationFactor: parseFloat(document.getElementById('degradation-factor').value) / 100,
    fatigueExponent: parseFloat(document.getElementById('fatigue-exponent').value),
    wallOnset: parseFloat(document.getElementById('wall-onset').value) / 100,
    downhillDegradation: parseFloat(document.getElementById('downhill-degradation').value) / 100,
});

const runAnalysis = () => {
    if (!analyzer) return;

    const { tolerance, minDistance, manualPaces, degradationFactor, fatigueExponent, wallOnset, downhillDegradation } = getManualModelInputs();

    try {
        const chunks = analyzer.analyze(
            tolerance,
            minDistance,
            manualPaces,
            degradationFactor,
            fatigueExponent,
            wallOnset,
            downhillDegradation
        );
        updateGrid(chunks);
        try {
            updatePreview(chunks);
        } catch (previewError) {
            console.warn('Route preview update failed:', previewError);
        }
    } catch (e) {
        showStatus('Analysis error: ' + e, 'error');
    }
};

const downloadCSV = () => {
    if (!analyzer) return;

    const { tolerance, minDistance, manualPaces, degradationFactor, fatigueExponent, wallOnset, downhillDegradation } = getManualModelInputs();

    try {
        const csvContent = analyzer.generate_csv(
            tolerance,
            minDistance,
            manualPaces,
            degradationFactor,
            fatigueExponent,
            wallOnset,
            downhillDegradation
        );
        const blob = new Blob([csvContent], { type: 'text/csv' });
        const url = window.URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = 'output.csv';
        document.body.appendChild(a);
        a.click();
        a.remove();
        window.URL.revokeObjectURL(url);
    } catch (e) {
        showStatus('CSV generation error: ' + e, 'error');
    }
};

// UI Listeners
document.getElementById('input-file').onchange = initAnalyzer;

// Update labels
document.getElementById('degradation-factor').oninput = (e) => {
    document.getElementById('degrad-val').textContent = e.target.value;
    runAnalysis();
};

document.getElementById('fatigue-exponent').oninput = (e) => {
    document.getElementById('fatigue-val').textContent = e.target.value;
    runAnalysis();
};

document.getElementById('wall-onset').oninput = (e) => {
    document.getElementById('wall-onset-val').textContent = e.target.value;
    runAnalysis();
};

document.getElementById('downhill-degradation').oninput = (e) => {
    document.getElementById('downhill-degrad-val').textContent = e.target.value;
    runAnalysis();
};

document.getElementById('hill-sensitivity').oninput = (e) => {
    document.getElementById('hill-sensitivity-val').textContent = e.target.value;
};

// Derive the 8 zone paces from the flat pace + hill sensitivity via the
// Minetti energy-cost model. Every field stays editable afterwards — this
// is a starting point, not a lock.
document.getElementById('apply-minetti-btn').onclick = () => {
    try {
        const flatSpm = parsePace(document.getElementById('p-flat').value);
        const hillSensitivity = parseFloat(document.getElementById('hill-sensitivity').value);
        const suggested = suggest_manual_paces(flatSpm, hillSensitivity);

        document.getElementById('p-gentle-up').value = formatPaceInput(suggested.gentle_uphill);
        document.getElementById('p-mod-up').value = formatPaceInput(suggested.moderate_uphill);
        document.getElementById('p-steep-up').value = formatPaceInput(suggested.steep_uphill);
        document.getElementById('p-vsteep-up').value = formatPaceInput(suggested.very_steep_uphill);
        document.getElementById('p-gentle-down').value = formatPaceInput(suggested.gentle_downhill);
        document.getElementById('p-mod-down').value = formatPaceInput(suggested.moderate_downhill);
        document.getElementById('p-steep-down').value = formatPaceInput(suggested.steep_downhill);
        document.getElementById('p-vsteep-down').value = formatPaceInput(suggested.very_steep_downhill);

        runAnalysis();
    } catch (e) {
        showStatus('Failed to compute suggested paces: ' + e, 'error');
    }
};

// Other inputs
['tolerance', 'min-distance'].forEach(id => {
    document.getElementById(id).onchange = runAnalysis;
});

// Manual pace inputs (flat pace lives outside .pace-grid now, seeding the
// Minetti model, but should still re-run analysis live like the others).
document.getElementById('p-flat').oninput = runAnalysis;
document.querySelectorAll('.pace-grid input').forEach(el => {
    el.oninput = runAnalysis;
});

downloadBtn.onclick = downloadCSV;

// Aid station form: add on button click or Enter, remove via the delegated
// list click handler (the list is fully re-rendered on every change).
const submitAidStation = () => {
    if (!routePoints.length) {
        showStatus('Load a GPX file before adding aid stations.', 'error');
        setTimeout(hideStatus, 2000);
        return;
    }
    const name = document.getElementById('aid-name').value.trim();
    const km = parseFloat(document.getElementById('aid-distance').value);
    if (!name || Number.isNaN(km)) {
        showStatus('Enter a name and a distance to add an aid station.', 'error');
        setTimeout(hideStatus, 2000);
        return;
    }
    addAidStation(name, km);
    document.getElementById('aid-name').value = '';
    document.getElementById('aid-distance').value = '';
};

document.getElementById('aid-add-btn').onclick = submitAidStation;
['aid-name', 'aid-distance'].forEach(id => {
    document.getElementById(id).addEventListener('keydown', (e) => {
        if (e.key === 'Enter') {
            e.preventDefault();
            submitAidStation();
        }
    });
});

document.getElementById('aid-station-list').addEventListener('click', (evt) => {
    const btn = evt.target.closest('.aid-remove-btn');
    if (!btn) return;
    removeAidStation(btn.dataset.id);
});

renderAidStationList();

// Start initialization
start();
