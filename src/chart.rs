use crate::format::{Unit, format_size};
use crate::query::SummaryEntry;
use crate::ui::{SummaryRow, TreemapTile};
use slint::{Color, Image, Rgba8Pixel, SharedPixelBuffer};
use std::f32::consts::{FRAC_PI_2, TAU};

const CHART_SIZE: u32 = 220;
const MAX_SLICES: usize = 10;
const MAX_TREEMAP_TILES: usize = 12;

const PALETTE: [[u8; 3]; 10] = [
    [0x3b, 0x82, 0xf6], // Blue
    [0x10, 0xb9, 0x81], // Emerald
    [0xf5, 0x9e, 0x0b], // Amber
    [0x8b, 0x5c, 0xf6], // Violet
    [0xec, 0x48, 0x99], // Rose
    [0x06, 0xb6, 0xd4], // Cyan
    [0xf9, 0x73, 0x16], // Orange
    [0x63, 0x66, 0xf1], // Indigo
    [0x14, 0xb8, 0xa6], // Teal
    [0xea, 0xb3, 0x08], // Gold
];
const OTHER_RGB: [u8; 3] = [0x64, 0x74, 0x8b];

pub fn palette_rgb(index: usize) -> [u8; 3] {
    if index < PALETTE.len() { PALETTE[index] } else { OTHER_RGB }
}

pub fn palette_color(index: usize) -> Color {
    let [r, g, b] = palette_rgb(index);
    Color::from_rgb_u8(r, g, b)
}

pub fn format_percent(share: f32) -> String {
    let pct = share * 100.0;
    if pct <= 0.0 {
        "0%".into()
    } else if pct < 0.1 {
        "<0.1%".into()
    } else if pct >= 99.95 {
        "100%".into()
    } else {
        format!("{pct:.1}%")
    }
}

pub fn build_rows(entries: &[SummaryEntry], unit: Unit) -> (Vec<SummaryRow>, u64) {
    let total: u64 = entries.iter().map(|e| e.size).sum();
    let largest = entries.iter().map(|e| e.size).max().unwrap_or(0).max(1) as f32;
    let total_f = total.max(1) as f32;
    let rows = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let share = if total > 0 { entry.size as f32 / total_f } else { 0.0 };
            SummaryRow {
                label: entry.label.as_str().into(),
                detail: entry.detail(unit).into(),
                percent: format_percent(share).into(),
                ratio: entry.size as f32 / largest,
                share,
                color: palette_color(index),
            }
        })
        .collect();
    (rows, total)
}

struct SliceSpan {
    index: usize,
    start: f32,
    end: f32,
    rgb: [u8; 3],
}

fn compute_spans(entries: &[SummaryEntry]) -> Vec<SliceSpan> {
    let total: u64 = entries.iter().map(|e| e.size).sum();
    if total == 0 || entries.is_empty() {
        return Vec::new();
    }
    let total_f = total as f32;
    let mut spans = Vec::new();
    let mut cursor = 0.0f32;
    let limit = entries.len().min(MAX_SLICES);
    for (i, entry) in entries.iter().take(limit).enumerate() {
        if entry.size == 0 {
            continue;
        }
        let is_last = i + 1 == limit && entries.len() <= MAX_SLICES;
        let delta = (entry.size as f32 / total_f) * TAU;
        let end = if is_last { TAU } else { (cursor + delta).min(TAU) };
        if end > cursor + 0.004 {
            spans.push(SliceSpan { index: i, start: cursor, end, rgb: palette_rgb(i) });
            cursor = end;
        }
    }
    if entries.len() > MAX_SLICES && cursor < TAU - 0.004 {
        spans.push(SliceSpan {
            index: MAX_SLICES,
            start: cursor,
            end: TAU,
            rgb: OTHER_RGB,
        });
    } else if let Some(last) = spans.last_mut() {
        last.end = TAU;
    }
    spans
}

pub fn hit_test_wheel(entries: &[SummaryEntry], donut: bool, nx: f32, ny: f32) -> Option<usize> {
    if nx.abs() > 1.1 || ny.abs() > 1.1 {
        return None;
    }
    let r = (nx * nx + ny * ny).sqrt();
    let r_min = if donut { 48.0 / 110.0 } else { 0.0 };
    let r_max = 102.0 / 110.0;
    if r < r_min || r > r_max {
        return None;
    }
    let spans = compute_spans(entries);
    if spans.is_empty() {
        return None;
    }
    let angle = (ny.atan2(nx) + FRAC_PI_2).rem_euclid(TAU);
    spans
        .iter()
        .find(|span| angle >= span.start && angle <= span.end)
        .map(|span| span.index.min(entries.len().saturating_sub(1)))
}

pub fn render_wheel(entries: &[SummaryEntry], donut: bool, hovered: Option<usize>) -> Image {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(CHART_SIZE, CHART_SIZE);
    let spans = compute_spans(entries);
    if spans.is_empty() {
        return Image::from_rgba8(buffer);
    }

    let hovered_slice = hovered.map(|idx| idx.min(MAX_SLICES));
    let multi = spans.len() > 1;
    let center = CHART_SIZE as f32 * 0.5;
    let pixels = buffer.make_mut_slice();

    for py in 0..CHART_SIZE {
        let dy0 = py as f32 + 0.5 - center;
        for px in 0..CHART_SIZE {
            let dx0 = px as f32 + 0.5 - center;
            let angle0 = (dy0.atan2(dx0) + FRAC_PI_2).rem_euclid(TAU);

            let Some(span) = spans.iter().find(|s| angle0 >= s.start && angle0 <= s.end) else {
                continue;
            };

            let is_hovered = hovered_slice == Some(span.index);
            let mid = 0.5 * (span.start + span.end);
            let (ox, oy) = if is_hovered && !donut {
                let shift = 5.0;
                (mid.sin() * shift, -mid.cos() * shift)
            } else {
                (0.0, 0.0)
            };

            let dx = dx0 - ox;
            let dy = dy0 - oy;
            let r = (dx * dx + dy * dy).sqrt();

            let r_outer = if is_hovered {
                if donut { 101.5 } else { 97.5 }
            } else {
                94.5
            };
            let r_inner = if donut {
                if is_hovered { 50.5 } else { 53.0 }
            } else {
                0.0
            };

            let outer_aa = (r_outer + 0.5 - r).clamp(0.0, 1.0);
            let inner_aa = if donut { (r - (r_inner - 0.5)).clamp(0.0, 1.0) } else { 1.0 };
            let mut alpha = outer_aa * inner_aa;
            if alpha <= 0.0 {
                continue;
            }

            if multi && r > 0.5 {
                let angle = (dy.atan2(dx) + FRAC_PI_2).rem_euclid(TAU);
                let d_start = (angle - span.start).abs().min(TAU - (angle - span.start).abs()) * r;
                let d_end = (span.end - angle).abs().min(TAU - (span.end - angle).abs()) * r;
                let gap_dist = d_start.min(d_end);
                let half_gap = 0.85;
                let gap_aa = (gap_dist - half_gap + 0.5).clamp(0.0, 1.0);
                alpha *= gap_aa;
            }

            if alpha <= 0.0 {
                continue;
            }

            let mut rgb = span.rgb;
            if is_hovered {
                rgb = [
                    rgb[0].saturating_add(24),
                    rgb[1].saturating_add(24),
                    rgb[2].saturating_add(24),
                ];
            } else if hovered_slice.is_some() {
                alpha *= 0.78;
            }

            // Subtle radial sheen for depth
            let t = ((r - r_inner) / (r_outer - r_inner).max(1.0)).clamp(0.0, 1.0);
            let sheen = (1.0 - t) * 14.0;
            let r_u8 = (rgb[0] as f32 + sheen).clamp(0.0, 255.0) as u8;
            let g_u8 = (rgb[1] as f32 + sheen).clamp(0.0, 255.0) as u8;
            let b_u8 = (rgb[2] as f32 + sheen).clamp(0.0, 255.0) as u8;
            let a_u8 = (alpha * 255.0).round() as u8;

            pixels[(py * CHART_SIZE + px) as usize] = Rgba8Pixel { r: r_u8, g: g_u8, b: b_u8, a: a_u8 };
        }
    }

    Image::from_rgba8(buffer)
}

pub fn build_treemap(entries: &[SummaryEntry], unit: Unit) -> Vec<TreemapTile> {
    let total: u64 = entries.iter().map(|e| e.size).sum();
    if total == 0 || entries.is_empty() {
        return Vec::new();
    }
    let total_f = total as f32;
    let limit = entries.len().min(MAX_TREEMAP_TILES);
    let mut items: Vec<(usize, String, String, String, f32, Color)> = entries
        .iter()
        .take(limit)
        .enumerate()
        .filter(|(_, e)| e.size > 0)
        .map(|(i, e)| {
            let share = e.size as f32 / total_f;
            (
                i,
                e.label.clone(),
                e.detail(unit),
                format_percent(share),
                e.size as f32,
                palette_color(i),
            )
        })
        .collect();

    if entries.len() > MAX_TREEMAP_TILES {
        let other_size: u64 = entries[MAX_TREEMAP_TILES..].iter().map(|e| e.size).sum();
        if other_size > 0 {
            let share = other_size as f32 / total_f;
            items.push((
                MAX_TREEMAP_TILES,
                "Other".into(),
                format_size(other_size, unit),
                format_percent(share),
                other_size as f32,
                Color::from_rgb_u8(OTHER_RGB[0], OTHER_RGB[1], OTHER_RGB[2]),
            ));
        }
    }

    let mut tiles = Vec::with_capacity(items.len());
    layout_treemap(&items, 0.0, 0.0, 1.0, 1.0, &mut tiles);
    tiles
}

fn layout_treemap(
    items: &[(usize, String, String, String, f32, Color)],
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    out: &mut Vec<TreemapTile>,
) {
    if items.is_empty() || w <= 0.0 || h <= 0.0 {
        return;
    }
    if items.len() == 1 {
        let (index, label, detail, percent, _, color) = &items[0];
        out.push(TreemapTile {
            index: *index as i32,
            label: label.as_str().into(),
            detail: detail.as_str().into(),
            percent: percent.as_str().into(),
            x,
            y,
            w,
            h,
            color: *color,
        });
        return;
    }

    let sum: f32 = items.iter().map(|item| item.4).sum();
    if sum <= 0.0 {
        return;
    }

    let half = sum * 0.5;
    let mut acc = 0.0f32;
    let mut split = 1;
    for (i, item) in items.iter().enumerate() {
        acc += item.4;
        if acc >= half {
            split = (i + 1).clamp(1, items.len() - 1);
            break;
        }
    }

    let first_sum: f32 = items[..split].iter().map(|item| item.4).sum();
    let ratio = (first_sum / sum).clamp(0.08, 0.92);

    if w >= h {
        let w1 = w * ratio;
        layout_treemap(&items[..split], x, y, w1, h, out);
        layout_treemap(&items[split..], x + w1, y, w - w1, h, out);
    } else {
        let h1 = h * ratio;
        layout_treemap(&items[..split], x, y, w, h1, out);
        layout_treemap(&items[split..], x, y + h1, w, h - h1, out);
    }
}

