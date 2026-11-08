use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub monitor: String,
    pub horizontal: bool,
    pub rightalign: bool,
    pub topalign: u8,
    pub vert_edge_padding: u32,
    pub horz_edge_padding: u32,
    pub avatar_size: u32,
    pub icon_spacing: u32,
    pub border_width: u32,
    pub icon_transparency: f32,
    pub highlight_self: bool,
    pub tk_col: [f32; 4],
    pub mt_col: [f32; 4],
    pub mt_bg_col: [f32; 4],
    pub fade_out_inactive: bool,
    pub inactive_time: u32,
    pub inactive_fade_time: u32,
    pub fade_out_limit: f32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            monitor: "Any".into(),
            horizontal: false,
            rightalign: false,
            topalign: 1,
            vert_edge_padding: 5,
            horz_edge_padding: 10,
            avatar_size: 30,
            icon_spacing: 8,
            border_width: 2,
            icon_transparency: 1.0,
            highlight_self: true,
            tk_col: [0.0, 0.7, 0.0, 1.0],
            mt_col: [0.6, 0.0, 0.0, 1.0],
            mt_bg_col: [0.0, 0.0, 0.0, 0.5],
            fade_out_inactive: true,
            inactive_time: 10,
            inactive_fade_time: 30,
            fade_out_limit: 0.5,
        }
    }
}

pub fn path() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(base.join("concord-overlay/config.toml"))
}

pub fn load(path: &Path) -> Result<Config, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn parse(text: &str) -> Result<Config, String> {
    let c: Config = toml::from_str(text).map_err(|e| e.to_string())?;
    let unit = |name: &str, v: f32| {
        if (0.0..=1.0).contains(&v) {
            Ok(())
        } else {
            Err(format!("`{name}` must be within 0.0..=1.0, got {v}"))
        }
    };
    let range = |name: &str, v: u32, lo: u32, hi: u32| {
        if (lo..=hi).contains(&v) {
            Ok(())
        } else {
            Err(format!("`{name}` must be within {lo}..={hi}, got {v}"))
        }
    };
    if c.monitor.is_empty() {
        return Err("`monitor` must be \"Any\" or an output name like \"HDMI-A-1\"".into());
    }
    range("topalign", c.topalign.into(), 0, 2)?;
    range("avatar_size", c.avatar_size, 8, 256)?;
    range("icon_spacing", c.icon_spacing, 0, 256)?;
    range("border_width", c.border_width, 0, 16)?;
    range("vert_edge_padding", c.vert_edge_padding, 0, 4096)?;
    range("horz_edge_padding", c.horz_edge_padding, 0, 4096)?;
    range("inactive_time", c.inactive_time, 0, 3600)?;
    range("inactive_fade_time", c.inactive_fade_time, 0, 3600)?;
    unit("icon_transparency", c.icon_transparency)?;
    unit("fade_out_limit", c.fade_out_limit)?;
    for (name, col) in [
        ("tk_col", c.tk_col),
        ("mt_col", c.mt_col),
        ("mt_bg_col", c.mt_bg_col),
    ] {
        for v in col {
            unit(name, v)?;
        }
    }
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_missing_give_defaults() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(
            load(Path::new("/nonexistent/concord-overlay.toml")).unwrap(),
            Config::default()
        );
        let d = Config::default();
        assert_eq!(
            (d.horizontal, d.rightalign, d.topalign, d.monitor.as_str()),
            (false, false, 1, "Any")
        );
        assert_eq!(
            (
                d.avatar_size,
                d.icon_spacing,
                d.border_width,
                d.horz_edge_padding
            ),
            (30, 8, 2, 10)
        );
    }

    #[test]
    fn shipped_example_is_valid_and_matches_defaults() {
        let text = include_str!("../config.example.toml");
        assert_eq!(parse(text).unwrap(), Config::default());
    }

    #[test]
    fn values_are_read() {
        let c = parse(
            "horizontal = true\ntopalign = 2\nmonitor = \"HDMI-A-1\"\ntk_col = [1, 0, 0, 1]\n",
        )
        .unwrap();
        assert!(c.horizontal);
        assert_eq!(c.topalign, 2);
        assert_eq!(c.monitor, "HDMI-A-1");
        assert_eq!(c.tk_col, [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn invalid_values_are_errors_naming_the_key() {
        for (text, key) in [
            ("topalign = 3", "topalign"),
            ("fade_out_limit = 1.5", "fade_out_limit"),
            ("mt_col = [0, 0, 2, 1]", "mt_col"),
            ("avatar_size = 4", "avatar_size"),
            ("icon_only = true", "icon_only"),
            ("rightalign = \"False\"", "rightalign"),
            ("tk_col = [0, 1, 0]", "tk_col"),
        ] {
            let err = parse(text).unwrap_err();
            assert!(err.contains(key), "{text:?} -> {err}");
        }
        assert!(parse("rightalign = False").is_err());
    }
}
