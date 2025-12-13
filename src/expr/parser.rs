#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    LParen,
    RParen,
    Atom(String),
}

#[derive(Debug)]
pub enum ParseError {
    UnexpectedEof,
    UnexpectedToken(String),
    UnbalancedParen,
}

#[derive(Debug, Clone)]
pub enum RawSexpr {
    Atom(String),
    List(Vec<RawSexpr>),
}

pub fn tokenize(exprs: String) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut buf = String::new();

    fn flush(tokens: &mut Vec<Token>, buf: &mut String) {
        if !buf.is_empty() {
            tokens.push(Token::Atom(std::mem::take(buf)));
        }
    }

    for ch in exprs.chars() {
        match ch {
            '(' => {
                flush(&mut tokens, &mut buf);
                tokens.push(Token::LParen);
            }
            ')' => {
                flush(&mut tokens, &mut buf);
                tokens.push(Token::RParen);
            }
            c if c.is_whitespace() => flush(&mut tokens, &mut buf),
            c => buf.push(c),
        }
    }
    flush(&mut tokens, &mut buf);
    tokens
}

pub fn parse_expr(toekns: Vec<Token>) -> Result<(RawSexpr, usize), ParseError> {
    fn parse_at(index: usize, tokens: Vec<Token>) -> Result<(RawSexpr, usize), ParseError> {
        let Some(token) = tokens.get(index) else {
            return Err(ParseError::UnexpectedEof);
        };

        match token {
            Token::LParen => {
                let mut inline_tokens: Vec<RawSexpr> = Vec::new();
                let mut inline_index = index;

                loop {
                    let Some(inline_token) = tokens.get(inline_index) else {
                        return Err(ParseError::UnexpectedEof);
                    };

                    if matches!(inline_token, Token::RParen) {
                        return Ok((RawSexpr::List(inline_tokens), inline_index + 1));
                    }

                    let (node, index) = parse_at(inline_index, tokens.clone())?;

                    //
                    inline_tokens.push(node);
                    inline_index = index
                }
            }
            Token::RParen => Err(ParseError::UnexpectedToken(")".into())),
            Token::Atom(expr) => Ok((RawSexpr::Atom(expr.clone()), index + 1)),
        }
    }

    parse_at(0, toekns)
}
