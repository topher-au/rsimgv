//! Text templates for overlay elements, with substitution in the style of mpv's `${property}`:
//!
//! - `${name}` is the value of the property `name`, or nothing while it has none.
//! - `${name:text}` is the value, or `text` while the property has none. The text is itself a template.
//! - `${?name:text}` is `text` only while the property has a value.
//! - `${!name:text}` is `text` only while the property has no value.
//! - `$$` is a dollar sign, and `\` before any character in a `text` makes it literal, such as `\}`.
//!
//! A property with an empty value counts as having none. A `${` that is never closed is plain text.

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Template(Vec<Node>);

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Text(String),
    /// `${name}` or `${name:fallback}`.
    Value { name: String, fallback: Template },
    /// `${?name:body}`, or with `negate`, `${!name:body}`.
    Condition { name: String, negate: bool, body: Template },
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    /// The nodes up to the end of the text, or up to the `}` that closes a `${` when `nested`. The `}` is consumed.
    /// `None` if a nested template reaches the end of the text without being closed.
    fn nodes(&mut self, nested: bool) -> Option<Vec<Node>> {
        let mut nodes = Vec::new();
        let mut text = String::new();
        while let Some(&c) = self.chars.get(self.at) {
            match c {
                '}' if nested => {
                    self.at += 1;
                    flush(&mut nodes, &mut text);
                    return Some(nodes);
                }
                '\\' if nested && self.at + 1 < self.chars.len() => {
                    text.push(self.chars[self.at + 1]);
                    self.at += 2;
                }
                '$' if self.chars.get(self.at + 1) == Some(&'$') => {
                    text.push('$');
                    self.at += 2;
                }
                '$' if self.chars.get(self.at + 1) == Some(&'{') => {
                    let start = self.at;
                    self.at += 2;
                    match self.property() {
                        Some(node) => {
                            flush(&mut nodes, &mut text);
                            nodes.push(node);
                        }
                        None => {
                            // Not closed, so the `${` is text and what follows is read again as the text it is.
                            self.at = start + 2;
                            text.push_str("${");
                        }
                    }
                }
                _ => {
                    text.push(c);
                    self.at += 1;
                }
            }
        }
        if nested {
            return None;
        }
        flush(&mut nodes, &mut text);
        Some(nodes)
    }

    /// The property after a `${`, up to its closing `}`.
    fn property(&mut self) -> Option<Node> {
        let negate_or_condition = match self.chars.get(self.at) {
            Some('?') => Some(false),
            Some('!') => Some(true),
            _ => None,
        };
        if negate_or_condition.is_some() {
            self.at += 1;
        }
        let mut name = String::new();
        loop {
            match *self.chars.get(self.at)? {
                '}' => {
                    self.at += 1;
                    let name = name.trim().to_owned();
                    return Some(match negate_or_condition {
                        Some(negate) => Node::Condition { name, negate, body: Template::default() },
                        None => Node::Value { name, fallback: Template::default() },
                    });
                }
                ':' => {
                    self.at += 1;
                    let body = Template(self.nodes(true)?);
                    let name = name.trim().to_owned();
                    return Some(match negate_or_condition {
                        Some(negate) => Node::Condition { name, negate, body },
                        None => Node::Value { name, fallback: body },
                    });
                }
                c => {
                    name.push(c);
                    self.at += 1;
                }
            }
        }
    }
}

fn flush(nodes: &mut Vec<Node>, text: &mut String) {
    if !text.is_empty() {
        nodes.push(Node::Text(std::mem::take(text)));
    }
}

impl Template {
    pub fn parse(source: &str) -> Self {
        let mut parser = Parser { chars: source.chars().collect(), at: 0 };
        Self(parser.nodes(false).unwrap_or_default())
    }

    /// The text the template makes, with each property's value from `lookup`.
    pub fn render(&self, lookup: &dyn Fn(&str) -> Option<String>) -> String {
        let mut out = String::new();
        self.render_into(&mut out, lookup);
        out
    }

    fn render_into(&self, out: &mut String, lookup: &dyn Fn(&str) -> Option<String>) {
        let value = |name: &str| lookup(name).filter(|value| !value.is_empty());
        for node in &self.0 {
            match node {
                Node::Text(text) => out.push_str(text),
                Node::Value { name, fallback } => match value(name) {
                    Some(value) => out.push_str(&value),
                    None => fallback.render_into(out, lookup),
                },
                Node::Condition { name, negate, body } => {
                    if value(name).is_some() != *negate {
                        body.render_into(out, lookup);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str) -> String {
        Template::parse(source).render(&|name| match name {
            "time" => Some("1:23".to_owned()),
            "speed" => Some("1.5".to_owned()),
            "empty" => Some(String::new()),
            _ => None,
        })
    }

    #[test]
    fn properties_are_replaced_by_their_values() {
        assert_eq!(render("${time} at ${speed}x"), "1:23 at 1.5x");
        assert_eq!(render("[${missing}]"), "[]");
    }

    #[test]
    fn a_fallback_stands_in_for_a_missing_or_empty_value() {
        assert_eq!(render("${missing:--:--}"), "--:--");
        assert_eq!(render("${empty:none}"), "none");
        assert_eq!(render("${time:never}"), "1:23");
        assert_eq!(render("${missing:${speed}}"), "1.5");
    }

    #[test]
    fn conditions_show_text_by_whether_a_value_is_there() {
        assert_eq!(render("${?speed:${speed}x}"), "1.5x");
        assert_eq!(render("${?missing:hidden}"), "");
        assert_eq!(render("${?empty:hidden}"), "");
        assert_eq!(render("${!missing:no value}"), "no value");
        assert_eq!(render("${!time:no value}"), "");
    }

    #[test]
    fn dollars_and_backslashes_escape() {
        assert_eq!(render("$$${time}"), "$1:23");
        assert_eq!(render("${missing:a\\}b}"), "a}b");
        assert_eq!(render("100%"), "100%");
    }

    #[test]
    fn an_unclosed_property_is_text() {
        assert_eq!(render("${time"), "${time");
        assert_eq!(render("${?missing:text ${time}"), "${?missing:text 1:23");
    }
}
