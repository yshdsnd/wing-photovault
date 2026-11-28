use std::fmt::Write as _;

#[derive(Debug, Clone)]
pub struct TimelineBar {
    pub name: String,
    pub first_date: String,
    pub last_date: String,
    pub shots: i64,
    pub active_days: i64,
}

#[derive(Debug, Clone)]
pub struct AnnualBar {
    pub year: i32,
    pub shots: i64,
}

#[derive(Debug, Clone, Default)]
pub struct TimelineSvgInput {
    pub title: String,
    pub subtitle: String,
    pub cameras: Vec<TimelineBar>,
    pub lenses: Vec<TimelineBar>,
    pub extenders: Vec<TimelineBar>,
    pub annual: Vec<AnnualBar>,
}

fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

fn parse_year_float(ymd: &str) -> Option<f64> {
    if ymd.len() < 10 {
        return None;
    }
    let y: i32 = ymd.get(0..4)?.parse().ok()?;
    let m: u32 = ymd.get(5..7)?.parse().ok()?;
    let d: u32 = ymd.get(8..10)?.parse().ok()?;
    if !(1970..=2999).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let month_offsets = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let day_of_year = month_offsets[(m - 1) as usize] + (d - 1);
    Some(y as f64 + (day_of_year as f64 / 365.0))
}

fn format_commas(n: i64) -> String {
    let neg = n < 0;
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    for (idx, ch) in digits.chars().rev().enumerate() {
        if idx > 0 && idx % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if neg {
        out.push('-');
    }
    out.chars().rev().collect()
}

fn camera_bar_color(name: &str) -> &'static str {
    let upper = name.to_ascii_uppercase();
    if upper.contains("EOS R") || upper.contains("ILCE") {
        if upper.contains("R7") || upper.contains("R10") || upper.contains("R50") {
            "#34D399"
        } else {
            "#22D3EE"
        }
    } else if upper.contains("5D") || upper.contains("6D") || upper.contains("1D") {
        "#818CF8"
    } else if upper.contains("EOS") || upper.contains("NIKON") {
        "#38BDF8"
    } else {
        "#94A3B8"
    }
}

fn lens_bar_color(name: &str) -> &'static str {
    let upper = name.to_ascii_uppercase();
    if upper.contains("100-500")
        || upper.contains("200-800")
        || upper.contains("100-400")
        || upper.contains("400MM")
        || upper.contains("600MM")
    {
        "#F59E0B"
    } else if upper.contains("70-200") || upper.contains("800MM") || upper.contains("60-600") {
        "#FB7185"
    } else if upper.contains("10-22")
        || upper.contains("14-35")
        || upper.contains("15-30")
        || upper.contains("16-35")
    {
        "#2DD4BF"
    } else if !upper.contains('-') {
        "#F472B6"
    } else {
        "#A78BFA"
    }
}

/// Render a standalone, XML-escaped dark-theme SVG timeline of cameras, lenses, extenders,
/// and annual shot counts aligned on the same horizontal year axis.
pub fn render_timeline_svg(input: &TimelineSvgInput) -> String {
    let mut min_yr = i32::MAX;
    let mut max_yr = i32::MIN;

    for b in input
        .cameras
        .iter()
        .chain(input.lenses.iter())
        .chain(input.extenders.iter())
    {
        if let Some(y) = parse_year_float(&b.first_date) {
            min_yr = min_yr.min(y.floor() as i32);
        }
        if let Some(y) = parse_year_float(&b.last_date) {
            max_yr = max_yr.max(y.floor() as i32);
        }
    }
    for a in &input.annual {
        min_yr = min_yr.min(a.year);
        max_yr = max_yr.max(a.year);
    }
    if min_yr > max_yr {
        min_yr = 2020;
        max_yr = 2026;
    }

    let min_x_yr = min_yr as f64;
    let max_x_yr = (max_yr + 1) as f64 + 0.25;

    let width: f64 = 1680.0;
    let left_pad: f64 = 410.0;
    let right_pad: f64 = 95.0;
    let chart_w = width - left_pad - right_pad;

    let row_h: f64 = 27.0;
    let sec_hdr_h: f64 = 34.0;
    let sec_gap: f64 = 22.0;

    let header_h: f64 = 112.0;
    let top_ticks_h: f64 = 24.0;

    let mut cur_y = header_h + top_ticks_h;
    let cam_sec_y = cur_y;
    let cam_start_y = cam_sec_y + sec_hdr_h;
    cur_y = cam_start_y + (input.cameras.len().max(1) as f64) * row_h + sec_gap;

    let lens_sec_y = cur_y;
    let lens_start_y = lens_sec_y + sec_hdr_h;
    cur_y = lens_start_y + (input.lenses.len().max(1) as f64) * row_h + sec_gap;

    let ext_sec_y = if input.extenders.is_empty() {
        None
    } else {
        let sy = cur_y;
        cur_y = sy + sec_hdr_h + (input.extenders.len() as f64) * row_h + sec_gap;
        Some(sy)
    };

    let annual_sec_y = cur_y;
    let annual_start_y = annual_sec_y + sec_hdr_h;
    let annual_h: f64 = 125.0;
    let grid_bottom = annual_start_y + annual_h;
    let height = grid_bottom + 64.0;

    let year_to_x = |yr: f64| -> f64 {
        left_pad + ((yr - min_x_yr) / (max_x_yr - min_x_yr).max(1.0)) * chart_w
    };

    let mut svg = String::with_capacity(16 * 1024);
    let _ = writeln!(
        svg,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.0} {height:.0}" width="{width:.0}" height="{height:.0}">"#
    );
    let _ = writeln!(
        svg,
        r#"  <style>
    text {{ font-family: "Segoe UI", "Yu Gothic UI", "Meiryo", -apple-system, sans-serif; }}
    .title {{ font-size: 24px; font-weight: 700; fill: #F8FAFC; }}
    .sub {{ font-size: 12.5px; fill: #94A3B8; }}
    .sec {{ font-size: 13.5px; font-weight: 700; fill: #F8FAFC; }}
    .lbl {{ font-size: 11.5px; fill: #CBD5E1; }}
    .lbl-major {{ font-size: 11.5px; font-weight: 700; fill: #F8FAFC; }}
    .cnt {{ font-size: 10.5px; font-weight: 700; fill: #94A3B8; }}
    .cnt-major {{ font-size: 10.5px; font-weight: 700; fill: #F8FAFC; }}
    .yr {{ font-size: 10.5px; font-weight: 700; fill: #CBD5E1; }}
    .barval {{ font-size: 9.5px; font-weight: 700; fill: #94A3B8; }}
    .footer {{ font-size: 10.5px; fill: #64748B; }}
  </style>"#
    );

    // Background
    let _ = writeln!(
        svg,
        r##"  <rect width="100%" height="100%" fill="#0B1120"/>"##
    );

    // Header
    let _ = writeln!(
        svg,
        r#"  <text x="36" y="42" class="title">{}</text>"#,
        escape_xml(&input.title)
    );
    let _ = writeln!(
        svg,
        r#"  <text x="38" y="68" class="sub">{}</text>"#,
        escape_xml(&input.subtitle)
    );

    // Year gridlines & ticks
    let span_years = (max_yr - min_yr + 1).max(1);
    for yr in min_yr..=(max_yr + 1) {
        let x = year_to_x(yr as f64);
        let stroke = if yr % 5 == 0 { "#475569" } else { "#1E293B" };
        let _ = writeln!(
            svg,
            r#"  <line x1="{x:.1}" y1="{:.1}" x2="{x:.1}" y2="{grid_bottom:.1}" stroke="{stroke}" stroke-width="1"/>"#,
            header_h + top_ticks_h
        );
        if yr <= max_yr {
            let x_mid = (x + year_to_x((yr + 1) as f64)) / 2.0;
            let show_full = span_years <= 14 || yr % 2 == 0 || yr == max_yr;
            let label = if show_full {
                yr.to_string()
            } else {
                format!("'{:02}", yr % 100)
            };
            let _ = writeln!(
                svg,
                r#"  <text x="{x_mid:.1}" y="{:.1}" text-anchor="middle" class="yr">{label}</text>"#,
                header_h + 15.0
            );
            let _ = writeln!(
                svg,
                r#"  <text x="{x_mid:.1}" y="{:.1}" text-anchor="middle" class="yr">{label}</text>"#,
                grid_bottom + 20.0
            );
        }
    }

    let draw_section = |out: &mut String, title: &str, y: f64, accent: &str| {
        let _ = writeln!(
            out,
            r#"  <rect x="36" y="{:.1}" width="5" height="19" rx="2" fill="{accent}"/>"#,
            y + 6.0
        );
        let _ = writeln!(
            out,
            r#"  <text x="50" y="{:.1}" class="sec">{}</text>"#,
            y + 20.0,
            escape_xml(title)
        );
        let _ = writeln!(
            out,
            r##"  <line x1="36" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="#334155" stroke-width="1"/>"##,
            y + 29.0,
            width - 36.0,
            y + 29.0
        );
    };

    let draw_rows = |out: &mut String,
                     rows: &[TimelineBar],
                     start_y: f64,
                     major_threshold: i64,
                     color_fn: fn(&str) -> &'static str| {
        if rows.is_empty() {
            let _ = writeln!(
                out,
                r#"  <text x="{:.1}" y="{:.1}" class="sub">(no items matching filter)</text>"#,
                left_pad + 8.0,
                start_y + 18.0
            );
            return;
        }
        for (idx, item) in rows.iter().enumerate() {
            let y = start_y + (idx as f64) * row_h;
            if idx % 2 == 1 {
                let _ = writeln!(
                    out,
                    r##"  <rect x="36" y="{y:.1}" width="{:.1}" height="{row_h:.1}" fill="#1E293B" fill-opacity="0.32"/>"##,
                    width - 72.0
                );
            }
            let is_major = item.shots >= major_threshold;
            let lbl_cls = if is_major { "lbl-major" } else { "lbl" };
            let _ = writeln!(
                out,
                r#"  <text x="{:.1}" y="{:.1}" text-anchor="end" class="{lbl_cls}">{}</text>"#,
                left_pad - 12.0,
                y + row_h * 0.68,
                escape_xml(&item.name)
            );

            let s_yr = parse_year_float(&item.first_date).unwrap_or(min_x_yr);
            let e_yr = parse_year_float(&item.last_date).unwrap_or(s_yr);
            let x0 = year_to_x(s_yr);
            let x1 = year_to_x(e_yr);
            let bw = (x1 - x0).max(8.0);
            let bh = if is_major { 17.0 } else { 13.0 };
            let by = y + (row_h - bh) / 2.0;
            let color = color_fn(&item.name);
            let stroke = if is_major {
                r##" stroke="#FFFFFF" stroke-opacity="0.45" stroke-width="1""##
            } else {
                ""
            };
            let _ = writeln!(
                out,
                r#"  <rect x="{x0:.1}" y="{by:.1}" width="{bw:.1}" height="{bh:.1}" rx="4" fill="{color}"{stroke}><title>{}: {} .. {} ({} shots, {} days)</title></rect>"#,
                escape_xml(&item.name),
                escape_xml(&item.first_date),
                escape_xml(&item.last_date),
                format_commas(item.shots),
                item.active_days
            );

            let cnt_cls = if is_major { "cnt-major" } else { "cnt" };
            let _ = writeln!(
                out,
                r#"  <text x="{:.1}" y="{:.1}" class="{cnt_cls}">{}</text>"#,
                x0 + bw + 7.0,
                y + row_h * 0.68,
                format_commas(item.shots)
            );
        }
    };

    // Sections
    draw_section(
        &mut svg,
        &format!("CAMERAS ({})", input.cameras.len()),
        cam_sec_y,
        "#22D3EE",
    );
    draw_rows(&mut svg, &input.cameras, cam_start_y, 10_000, camera_bar_color);

    draw_section(
        &mut svg,
        &format!("LENSES ({})", input.lenses.len()),
        lens_sec_y,
        "#F59E0B",
    );
    draw_rows(&mut svg, &input.lenses, lens_start_y, 10_000, lens_bar_color);

    if let Some(sy) = ext_sec_y {
        draw_section(
            &mut svg,
            &format!("EXTENDERS ({})", input.extenders.len()),
            sy,
            "#EAB308",
        );
        draw_rows(&mut svg, &input.extenders, sy + sec_hdr_h, 2_500, |_| "#EAB308");
    }

    // Annual shots bar chart
    draw_section(
        &mut svg,
        &format!("ANNUAL SHOTS ({min_yr} - {max_yr})"),
        annual_sec_y,
        "#38BDF8",
    );
    let max_annual = input
        .annual
        .iter()
        .map(|a| a.shots)
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let _ = writeln!(
        svg,
        r##"  <line x1="{left_pad:.1}" y1="{grid_bottom:.1}" x2="{:.1}" y2="{grid_bottom:.1}" stroke="#64748B" stroke-width="1.2"/>"##,
        width - right_pad
    );
    for a in &input.annual {
        let x0 = year_to_x(a.year as f64 + 0.14);
        let x1 = year_to_x(a.year as f64 + 0.86);
        let bw = (x1 - x0).max(4.0);
        let bh = ((a.shots as f64) / max_annual) * (annual_h - 22.0);
        let by = grid_bottom - bh.max(2.0);
        let _ = writeln!(
            svg,
            r##"  <rect x="{x0:.1}" y="{by:.1}" width="{bw:.1}" height="{:.1}" rx="3" fill="#38BDF8" fill-opacity="0.88"><title>{}: {} shots</title></rect>"##,
            bh.max(2.0),
            a.year,
            format_commas(a.shots)
        );
        let val_lbl = if a.shots >= 1_000 {
            format!("{:.1}k", a.shots as f64 / 1000.0)
        } else {
            a.shots.to_string()
        };
        let _ = writeln!(
            svg,
            r#"  <text x="{:.1}" y="{:.1}" text-anchor="middle" class="barval">{val_lbl}</text>"#,
            x0 + bw / 2.0,
            by - 4.0
        );
    }

    // Footer
    let _ = writeln!(
        svg,
        r#"  <text x="36" y="{:.1}" class="footer">Generated by WING PhotoVault</text>"#,
        height - 18.0
    );
    let _ = writeln!(svg, "</svg>");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_valid_xml_escaped_svg_and_handles_empty_input() {
        let empty = render_timeline_svg(&TimelineSvgInput {
            title: "Empty <Test> & \"Quotes\"".into(),
            subtitle: "0 assets".into(),
            ..Default::default()
        });
        assert!(empty.starts_with("<svg "));
        assert!(empty.trim_end().ends_with("</svg>"));
        assert!(empty.contains("Empty &lt;Test&gt; &amp; &quot;Quotes&quot;"));
        assert!(!empty.contains("<Test>"));

        let populated = render_timeline_svg(&TimelineSvgInput {
            title: "Camera History".into(),
            subtitle: "100 assets".into(),
            cameras: vec![TimelineBar {
                name: "Canon EOS R5m2".into(),
                first_date: "2024-10-28".into(),
                last_date: "2026-11-22".into(),
                shots: 73_074,
                active_days: 58,
            }],
            lenses: vec![TimelineBar {
                name: "RF100-500mm F4.5-7.1 L IS USM".into(),
                first_date: "2021-09-18".into(),
                last_date: "2026-11-03".into(),
                shots: 64_862,
                active_days: 57,
            }],
            extenders: vec![TimelineBar {
                name: "EXTENDER RF1.4x".into(),
                first_date: "2021-09-19".into(),
                last_date: "2025-09-21".into(),
                shots: 5_843,
                active_days: 9,
            }],
            annual: vec![
                AnnualBar { year: 2024, shots: 28_877 },
                AnnualBar { year: 2025, shots: 32_832 },
            ],
        });
        assert!(populated.contains("EXTENDERS (1)"));
        assert!(populated.contains("73,074"));
        assert!(populated.contains("32.8k"));
    }
}
