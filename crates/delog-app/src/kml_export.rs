//! KML (Google Earth) export of vehicle trajectories: one `<Placemark>` with a
//! `<gx:MultiTrack>` per vehicle, timestamps anchored to UTC when the log
//! carries an anchor field.

use egui::Color32;

use crate::vehicle::GeodeticSample;

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// KML colors are `aabbggrr` hex — reversed byte order from RGBA.
fn kml_color(color: Color32) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        color.a(),
        color.b(),
        color.g(),
        color.r()
    )
}

/// Days since 1970-01-01 → `(year, month, day)` (proleptic Gregorian).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Unix µs → ISO 8601 UTC with millisecond precision.
fn format_iso8601_utc(unix_us: i64) -> String {
    let secs = unix_us.div_euclid(1_000_000);
    let millis = unix_us.rem_euclid(1_000_000) / 1_000;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Split a gap-marked point stream into contiguous segments, dropping gaps.
fn split_segments(points: Vec<Option<GeodeticSample>>) -> Vec<Vec<GeodeticSample>> {
    let mut segments = Vec::new();
    let mut current = Vec::new();
    for p in points {
        match p {
            Some(p) => current.push(p),
            None if !current.is_empty() => segments.push(std::mem::take(&mut current)),
            None => {}
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(t_us: i64) -> GeodeticSample {
        GeodeticSample {
            t_us,
            lat_deg: 47.5,
            lon_deg: 8.25,
            alt_m: 400.0,
        }
    }

    #[test]
    fn xml_escape_replaces_markup_characters() {
        assert_eq!(xml_escape("<A & \"B's\">"), "&lt;A &amp; &quot;B&apos;s&quot;&gt;");
    }

    #[test]
    fn kml_color_is_aabbggrr() {
        let c = Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 0xff);
        assert_eq!(kml_color(c), "ff332211");
    }

    #[test]
    fn iso8601_formats_epoch_and_known_dates() {
        assert_eq!(format_iso8601_utc(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            format_iso8601_utc(1_600_000_000_000_000),
            "2020-09-13T12:26:40.000Z"
        );
        assert_eq!(format_iso8601_utc(-1_000), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn split_segments_breaks_on_gaps_and_drops_empty_runs() {
        let pts = vec![None, Some(sample(0)), None, Some(sample(1)), Some(sample(2)), None];
        let segs = split_segments(pts);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].len(), 1);
        assert_eq!(segs[1].len(), 2);
        assert!(split_segments(vec![None, None]).is_empty());
    }
}
