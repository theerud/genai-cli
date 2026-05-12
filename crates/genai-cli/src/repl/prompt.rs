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
            s.push_str(r);
        }
        s.push_str("> ");
        s
    }
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
        assert_eq!(PromptState { role: None, session: Some("s") }.render(), "*> ");
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
}
