pub struct PromptState<'a> {
    pub role: Option<&'a str>,
    pub session: Option<&'a str>,
}

impl<'a> PromptState<'a> {
    pub fn render(&self) -> String {
        let mut s = String::new();
        if self.session.is_some() {
            s.push('*');
        }
        if let Some(r) = self.role {
            s.push_str(&truncate(r, 16));
        } else if let Some(sess) = self.session {
            // Temporary sessions pass "*" through; named ones get a compact label.
            if sess != "*" {
                s.push_str(&truncate(sess, 16));
            }
        }
        s.push_str("> ");
        s
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = chars.into_iter().take(keep).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_arrow() {
        assert_eq!(PromptState { role: None, session: None }.render(), "> ");
    }

    #[test]
    fn star_when_session() {
        assert_eq!(PromptState { role: None, session: Some("s") }.render(), "*s> ");
    }

    #[test]
    fn role_prefixed() {
        assert_eq!(PromptState { role: Some("coding"), session: None }.render(), "coding> ");
    }

    #[test]
    fn role_and_session() {
        assert_eq!(
            PromptState { role: Some("coding"), session: Some("x") }.render(),
            "*coding> "
        );
    }

    #[test]
    fn truncates_long_label() {
        assert_eq!(
            PromptState { role: None, session: Some("averyveryverylongname") }.render(),
            "*averyveryverylo…> "
        );
    }
}
