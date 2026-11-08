use serde::Deserialize;
use serde::de::IgnoredAny;

#[derive(Debug, Deserialize)]
pub struct State {
    pub v: u32,
    pub self_id: Option<String>,
    pub channel: Option<IgnoredAny>,
    #[serde(default)]
    pub members: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub avatar: Option<String>,
    #[serde(default)]
    pub speaking: bool,
    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub deaf: bool,
    #[serde(default)]
    pub self_mute: bool,
    #[serde(default)]
    pub self_deaf: bool,
}

impl Member {
    pub fn muted(&self) -> bool {
        self.mute || self.self_mute
    }
    pub fn deafened(&self) -> bool {
        self.deaf || self.self_deaf
    }
}

pub fn parse_line(line: &[u8]) -> Result<State, String> {
    let state: State =
        serde_json::from_slice(line).map_err(|e| format!("bad line from concord: {e}"))?;
    if state.v != 1 {
        return Err(format!(
            "unsupported concord-voice protocol v{} (this overlay speaks v1)",
            state.v
        ));
    }
    Ok(state)
}

pub fn visible(state: State, highlight_self: bool) -> Vec<Member> {
    if state.channel.is_none() {
        return Vec::new();
    }
    let me = state.self_id.as_deref().filter(|_| highlight_self);
    let mut members = state.members;
    members.sort_by_cached_key(|m| {
        (
            Some(m.id.as_str()) != me,
            m.name.to_lowercase(),
            m.id.clone(),
        )
    });
    members
}

#[derive(Default)]
pub struct LineBuf(Vec<u8>);

const MAX_LINE: usize = 1 << 20;

impl LineBuf {
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        self.0.extend_from_slice(data);
        let mut lines = Vec::new();
        while let Some(pos) = self.0.iter().position(|&b| b == b'\n') {
            let rest = self.0.split_off(pos + 1);
            let mut line = std::mem::replace(&mut self.0, rest);
            line.pop();
            if !line.is_empty() {
                lines.push(line);
            }
        }
        if self.0.len() > MAX_LINE {
            return Err("line from concord longer than 1 MiB".into());
        }
        Ok(lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"v":1,"self_id":"123","channel":{"id":"456","name":"General","guild_id":"789"},"members":[{"id":"123","name":"me","avatar":"https://cdn.discordapp.com/avatars/123/abc.png","speaking":false,"mute":false,"deaf":false,"self_mute":false,"self_deaf":false,"stream":false}]}"#;

    #[test]
    fn parses_the_protocol_example() {
        let s = parse_line(LINE.as_bytes()).unwrap();
        assert_eq!(s.self_id.as_deref(), Some("123"));
        let m = visible(s, true);
        assert_eq!(m.len(), 1);
        assert_eq!(
            m[0].avatar.as_deref(),
            Some("https://cdn.discordapp.com/avatars/123/abc.png")
        );
        assert!(!m[0].muted() && !m[0].deafened() && !m[0].speaking);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let line = r#"{"v":1,"new_top":[1,2],"self_id":null,"channel":{"id":"1","topic":"x"},
            "members":[{"id":"9","name":"a","avatar":null,"speaking":true,"self_deaf":true,"video":{"on":true}}]}"#;
        let m = visible(parse_line(line.as_bytes()).unwrap(), true);
        assert_eq!(m.len(), 1);
        assert!(m[0].speaking && m[0].deafened() && !m[0].muted());
    }

    #[test]
    fn null_channel_shows_nobody() {
        let s = parse_line(br#"{"v":1,"self_id":"1","channel":null,"members":[]}"#).unwrap();
        assert!(visible(s, true).is_empty());
    }

    #[test]
    fn bad_lines_are_errors() {
        assert!(parse_line(b"{not json").is_err());
        assert!(
            parse_line(br#"{"v":2,"self_id":null,"channel":null,"members":[]}"#)
                .unwrap_err()
                .contains("v2")
        );
        assert!(parse_line(br#"{"v":1,"channel":{},"members":[{"name":"no id"}]}"#).is_err());
    }

    #[test]
    fn self_first_then_alphabetical() {
        let line = r#"{"v":1,"self_id":"5","channel":{},"members":[
            {"id":"1","name":"zed","avatar":null},{"id":"2","name":"Bob","avatar":null},
            {"id":"5","name":"me","avatar":null},{"id":"3","name":"alice","avatar":null},
            {"id":"0","name":"bob","avatar":null}]}"#;
        let ids = |hs| {
            visible(parse_line(line.as_bytes()).unwrap(), hs)
                .into_iter()
                .map(|m| m.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(true), ["5", "3", "0", "2", "1"]);
        assert_eq!(ids(false), ["3", "0", "2", "5", "1"]);
    }

    #[test]
    fn lines_and_utf8_split_across_reads() {
        let line = r#"{"v":1,"self_id":"1","channel":{},"members":[{"id":"1","name":"Алиса 🎧","avatar":null}]}"#;
        let full = format!("{line}\n{line}\n");
        let bytes = full.as_bytes();
        let cut = line.find('л').unwrap() + 1;
        let mut buf = LineBuf::default();
        assert!(buf.push(&bytes[..cut]).unwrap().is_empty());
        let emoji = line.find('🎧').unwrap() + 2;
        let lines = buf.push(&bytes[cut..line.len() + 1 + emoji]).unwrap();
        assert_eq!(lines.len(), 1);
        let lines2 = buf.push(&bytes[line.len() + 1 + emoji..]).unwrap();
        assert_eq!(lines2.len(), 1);
        for l in lines.iter().chain(&lines2) {
            let m = visible(parse_line(l).unwrap(), true);
            assert_eq!(m[0].name, "Алиса 🎧");
        }
    }

    #[test]
    fn overlong_line_is_an_error() {
        let mut buf = LineBuf::default();
        assert!(buf.push(&vec![b'x'; MAX_LINE + 1]).is_err());
    }
}
