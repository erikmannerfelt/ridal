//! Tokenizer for step lists such as `dewow(10), bandpass(0.1 0.9 q=0.5)`.
//!
//! This only finds the structure: step names, their arguments and where
//! each sits in the source. It knows nothing about which steps exist or
//! what their arguments mean -- that is the registry's job (see
//! [`super::Step`]), which keeps the two failure modes apart: "this is not
//! a step list" here, "this is not a valid step" there.
//!
//! Grammar:
//!
//! ```text
//! steps := step (',' step)*
//! step  := word ( '(' args? ')' )?
//! args  := arg ( ','? arg )*        -- commas or whitespace between arguments
//! arg   := word | word '=' word
//! ```
//!
//! A comma at depth 0 separates steps and a comma inside parentheses
//! separates arguments, so `subset(0, 200)` means one step with two
//! arguments (#10). Whitespace at depth 0 between two steps is an error
//! rather than a silently dropped step (#130).

/// A byte range into the parsed source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

/// One argument as written: positional (`10`) or named (`window=10`).
#[derive(Debug, Clone, PartialEq)]
pub struct RawArg {
    pub key: Option<String>,
    pub value: String,
    pub span: Span,
}

/// One step as written, before anything checks that it exists.
#[derive(Debug, Clone, PartialEq)]
pub struct RawStep {
    pub name: String,
    pub args: Vec<RawArg>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SyntaxError {
    pub message: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),
    LParen,
    RParen,
    Comma,
    Eq,
}

/// Characters that end a word. Everything else, including `-` and `.`, is
/// part of one, so `-1`, `0.5` and `3-7` (a `remove_traces` range) are
/// single words.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | ',' | '=')
}

fn lex(source: &str) -> Vec<(Token, Span)> {
    let mut tokens = Vec::new();
    let mut chars = source.char_indices().peekable();
    while let Some((start, c)) = chars.next() {
        let token = match c {
            _ if c.is_whitespace() => continue,
            '(' => Token::LParen,
            ')' => Token::RParen,
            ',' => Token::Comma,
            '=' => Token::Eq,
            _ => {
                let mut end = start + c.len_utf8();
                while let Some(&(i, next)) = chars.peek() {
                    if is_delimiter(next) {
                        break;
                    }
                    end = i + next.len_utf8();
                    chars.next();
                }
                tokens.push((
                    Token::Word(source[start..end].to_string()),
                    Span { start, end },
                ));
                continue;
            }
        };
        tokens.push((
            token,
            Span {
                start,
                end: start + 1,
            },
        ));
    }
    tokens
}

/// Split a step list into its steps.
///
/// # Examples
///
/// ```ignore
/// let steps = parse_step_list("dewow(10), subset(0, 200)").unwrap();
/// assert_eq!(steps[1].args.len(), 2);
/// ```
pub fn parse_step_list(source: &str) -> Result<Vec<RawStep>, SyntaxError> {
    let tokens = lex(source);
    let end_of_input = Span {
        start: source.len(),
        end: source.len(),
    };
    let mut pos = 0;
    let mut steps = Vec::new();

    loop {
        let (name, name_span) = match tokens.get(pos) {
            Some((Token::Word(w), span)) => (w.clone(), *span),
            Some((_, span)) => {
                return Err(SyntaxError {
                    message: "expected a step name".into(),
                    span: *span,
                })
            }
            None => {
                return Err(SyntaxError {
                    message: if steps.is_empty() {
                        "no steps given".into()
                    } else {
                        "expected a step name after the trailing comma".into()
                    },
                    span: end_of_input,
                })
            }
        };
        pos += 1;

        let mut args = Vec::new();
        let mut step_end = name_span.end;
        if let Some((Token::LParen, _)) = tokens.get(pos) {
            pos += 1;
            let mut after_separator = true;
            loop {
                match tokens.get(pos) {
                    Some((Token::RParen, span)) => {
                        step_end = span.end;
                        pos += 1;
                        break;
                    }
                    None => {
                        return Err(SyntaxError {
                            message: format!("`{name}(` is never closed with `)`"),
                            span: Span {
                                start: name_span.start,
                                end: source.len(),
                            },
                        })
                    }
                    Some((Token::Comma, span)) => {
                        if after_separator {
                            return Err(SyntaxError {
                                message: "empty argument".into(),
                                span: *span,
                            });
                        }
                        after_separator = true;
                        pos += 1;
                    }
                    Some((Token::LParen, span)) => {
                        return Err(SyntaxError {
                            message: "parentheses cannot be nested inside step arguments".into(),
                            span: *span,
                        })
                    }
                    Some((Token::Eq, span)) => {
                        return Err(SyntaxError {
                            message: "`=` needs an argument name before it, as in `window=10`"
                                .into(),
                            span: *span,
                        })
                    }
                    Some((Token::Word(word), span)) => {
                        let (key, value, arg_span) = match tokens.get(pos + 1) {
                            Some((Token::Eq, eq_span)) => match tokens.get(pos + 2) {
                                Some((Token::Word(value), value_span)) => {
                                    pos += 3;
                                    (
                                        Some(word.clone()),
                                        value.clone(),
                                        Span {
                                            start: span.start,
                                            end: value_span.end,
                                        },
                                    )
                                }
                                _ => {
                                    return Err(SyntaxError {
                                        message: format!("`{word}=` needs a value after it"),
                                        span: *eq_span,
                                    })
                                }
                            },
                            _ => {
                                pos += 1;
                                (None, word.clone(), *span)
                            }
                        };
                        args.push(RawArg {
                            key,
                            value,
                            span: arg_span,
                        });
                        after_separator = false;
                    }
                }
            }
        }

        steps.push(RawStep {
            name,
            args,
            span: Span {
                start: name_span.start,
                end: step_end,
            },
        });

        match tokens.get(pos) {
            None => return Ok(steps),
            Some((Token::Comma, _)) => pos += 1,
            Some((Token::Word(next), span)) => {
                return Err(SyntaxError {
                    message: format!(
                        "steps are separated by commas: add one before `{next}` (arguments go inside the parentheses)"
                    ),
                    span: *span,
                })
            }
            Some((Token::RParen, span)) => {
                return Err(SyntaxError {
                    message: "`)` without a matching `(`".into(),
                    span: *span,
                })
            }
            Some((_, span)) => {
                return Err(SyntaxError {
                    message: "expected `,` or the end of the step list".into(),
                    span: *span,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names_and_args(source: &str) -> Vec<(String, Vec<(Option<String>, String)>)> {
        parse_step_list(source)
            .unwrap()
            .into_iter()
            .map(|s| {
                (
                    s.name,
                    s.args.into_iter().map(|a| (a.key, a.value)).collect(),
                )
            })
            .collect()
    }

    fn positional(values: &[&str]) -> Vec<(Option<String>, String)> {
        values.iter().map(|v| (None, v.to_string())).collect()
    }

    #[test]
    fn commas_inside_parentheses_separate_arguments_not_steps() {
        // #10: this used to become the steps `subset(0` and `200)`.
        assert_eq!(
            names_and_args("subset(0, 200), dewow"),
            vec![
                ("subset".into(), positional(&["0", "200"])),
                ("dewow".into(), vec![]),
            ]
        );
    }

    #[test]
    fn spaces_and_commas_are_interchangeable_between_arguments() {
        assert_eq!(
            names_and_args("subset(0 -1, 0   500)"),
            names_and_args("subset(0, -1, 0, 500)")
        );
    }

    #[test]
    fn named_arguments_allow_spaces_around_the_equals_sign() {
        let expected = vec![(
            "bandpass".to_string(),
            vec![
                (None, "0.1".to_string()),
                (Some("q".to_string()), "0.5".to_string()),
            ],
        )];
        assert_eq!(names_and_args("bandpass(0.1 q=0.5)"), expected);
        assert_eq!(names_and_args("bandpass(0.1, q = 0.5)"), expected);
    }

    #[test]
    fn ranges_and_negative_numbers_are_single_arguments() {
        assert_eq!(
            names_and_args("remove_traces(1 3-7 -1)"),
            vec![("remove_traces".into(), positional(&["1", "3-7", "-1"]))]
        );
    }

    #[test]
    fn whitespace_around_names_and_parentheses_is_ignored() {
        assert_eq!(
            names_and_args("  kirchhoff_migration2d    (1    -2)    "),
            vec![("kirchhoff_migration2d".into(), positional(&["1", "-2"]))]
        );
    }

    #[test]
    fn a_space_between_steps_is_an_error_not_a_dropped_step() {
        // #130: "dewow bandpass" used to run one step.
        let err = parse_step_list("dewow bandpass").unwrap_err();
        assert!(
            err.message.contains("separated by commas"),
            "{}",
            err.message
        );
        assert_eq!(err.span, Span { start: 6, end: 14 });
    }

    #[test]
    fn malformed_lists_are_rejected_with_a_location() {
        for (source, fragment) in [
            ("", "no steps"),
            ("dewow,", "trailing comma"),
            ("dewow(10", "never closed"),
            ("dewow(10))", "without a matching"),
            ("subset(0,,200)", "empty argument"),
            ("dewow(window=)", "needs a value"),
            ("dewow(=5)", "argument name"),
            ("dewow((5))", "nested"),
        ] {
            let err = parse_step_list(source).unwrap_err();
            assert!(
                err.message.contains(fragment),
                "{source:?}: expected {fragment:?} in {:?}",
                err.message
            );
            assert!(err.span.end <= source.len());
        }
    }
}
