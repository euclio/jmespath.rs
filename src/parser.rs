//! JMESPath parser and AST
extern crate rustc_serialize;

use std::iter::Peekable;
use self::rustc_serialize::json::{Json};

use ast::*;
use lexer::Lexer;
use lexer::Token;

pub type ParseResult = Result<Ast, ParseError>;

/// Parses a JMESPath expression into an AST
pub fn parse(expr: &str) -> ParseResult {
    Parser::new(expr).parse()
}

/// Encountered when an invalid JMESPath expression is parsed.
#[derive(Clone, PartialEq, Debug)]
pub struct ParseError {
    /// The error message.
    msg: String,
    /// The line number of the error.
    line: usize,
    /// The column of the error.
    col: usize,
}

impl ParseError {
    pub fn new(expr: &str, pos: usize, msg: &str, hint: &str) -> ParseError {
        // Find each new line and create a formatted error message.
        let mut line: usize = 0;
        let mut col: usize = 0;
        let mut buff = String::new();
        let mut placed = false;
        for l in expr.lines().collect::<Vec<&str>>() {
            buff.push_str(l);
            buff.push('\n');
            if placed { continue; }
            if buff.len() > pos {
                placed = true;
                col = match line {
                    0 => pos,
                    _ => buff.len().checked_sub(2 + pos).unwrap_or(0)
                };
                ParseError::inject_err_pointer(&mut buff, col);
            } else {
                line += 1;
            }
        }
        if hint.len() > 0 { buff.push_str(&format!("Hint: {}", hint)); }
        ParseError {
            msg: format!("Parse error at line {}, col {}; {}\n{}", line, col, msg, buff),
            line: line,
            col: col
        }
    }

    fn inject_err_pointer(string_buffer: &mut String, col: usize) {
        let span = (0..col).map(|_| ' ').collect::<String>();
        string_buffer.push_str(&span);
        string_buffer.push_str(&"^\n");
    }
}

/// JMESPath parser. Returns an Ast
pub struct Parser<'a> {
    /// Peekable token stream
    stream: Peekable<Lexer<'a>>,
    /// Expression being parsed
    expr: String,
    /// The current token
    token: Token,
    /// The current character offset in the expression
    pos: usize,
}

impl<'a> Parser<'a> {
    // Constructs a new lexer using the given expression string.
    pub fn new(expr: &'a str) -> Parser<'a> {
        let mut lexer = Lexer::new(expr);
        let tok0 = lexer.next().unwrap_or(Token::Eof);
        Parser {
            stream: lexer.peekable(),
            expr: expr.to_string(),
            token: tok0,
            pos: 0,
        }
    }

    /// Parses the expression into result containing an AST or ParseError.
    pub fn parse(&mut self) -> ParseResult {
        // Skip leading whitespace
        if self.token.is_whitespace() {
            self.advance();
        }
        self.expr(0)
            .and_then(|result| {
                // After parsing the expr, we should reach the end of the stream.
                match self.stream.next() {
                    None | Some(Token::Eof) => Ok(result),
                    _ => Err(self.err(&"Did not reach token stream EOF"))
                }
            })
    }

    /// Ensures that the next token in the token stream is one of the pipe
    /// separated token named provided as the edible argument (e.g.,
    /// "Identifier|Eof").
    fn expect(&mut self, edible: &str) -> ParseResult {
        self.advance();
        // Get the string name of the token.
        if edible.contains(&self.token.token_name()) {
            Ok(CurrentNode)
        } else {
            Err(self.err(&format!("Expected {:?}", edible)))
        }
    }

    /// Advances the cursor position, skipping any whitespace encountered.
    fn advance(&mut self) {
        loop {
            self.pos += self.token.span();
            self.token = self.stream.next().unwrap_or(Token::Eof);
            if !self.token.is_whitespace() {
                break;
            }
        }
    }

    /// Main parse function of the Pratt parser that parses while RBP < LBP
    pub fn expr(&mut self, rbp: usize) -> ParseResult {
        // Parse the nud token.
        let mut left = match self.token.clone() {
            Token::At => self.nud_at(),
            Token::Identifier { value, .. } => self.nud_identifier(value),
            Token::Star => self.nud_star(),
            Token::Lbracket => self.nud_lbracket(),
            Token::Flatten => self.nud_flatten(),
            Token::Literal { value, ..} => self.nud_literal(value),
            Token::Lbrace => self.nud_lbrace(),
            Token::Ampersand => self.nud_ampersand(),
            // Token::Filter => self.nud_filter(),
            _ => return Err(self.token_err()),
        };

        // Parse any led tokens with a higher binding power.
        while rbp < self.token.lbp() {
            left = match self.token {
                Token::Dot => self.led_dot(try!(left)),
                Token::Lbracket => self.led_lbracket(try!(left)),
                Token::Flatten => self.led_flatten(try!(left)),
                Token::Or => self.led_or(try!(left)),
                Token::Pipe => self.led_pipe(try!(left)),
                Token::Lparen => self.led_lparen(try!(left)),
                _ => return Err(self.token_err()),
            };
        }

        left
    }

    /// Returns a formatted ParseError with the given message.
    fn err(&self, msg: &str) -> ParseError {
        let hint_msg = match self.token.clone() {
            Token::Unknown { hint, .. } => hint,
            _ => "".to_string()
        };
        ParseError::new(&self.expr, self.pos, msg, &hint_msg)
    }

    /// Generates a formatted parse error for an out of place token.
    fn token_err(&self) -> ParseError {
        self.err(&format!("Unexpected token: {}", self.token.token_name()))
    }

    /// Examples: &foo
    fn nud_ampersand(&mut self) -> ParseResult {
        self.advance();
        let rhs = try!(self.expr(Token::Ampersand.lbp()));
        Ok(Expref(Box::new(rhs)))
    }

    /// Examples: "@"
    fn nud_at(&mut self) -> ParseResult {
        self.advance();
        Ok(Ast::CurrentNode)
    }

    /// Examples: "Foo"
    fn nud_identifier(&mut self, s: String) -> ParseResult {
        self.advance();
        Ok(Ast::Identifier(s))
    }

    /// Examples: "[0]", "[*]", "[a, b]", "[0:1]", etc...
    fn nud_lbracket(&mut self) -> ParseResult {
        self.advance();
        match self.token {
            Token::Number { .. } | Token::Colon => self.parse_array_index(),
            Token::Star => {
                if self.stream.peek() != Some(&Token::Rbracket) {
                    return self.parse_multi_list();
                }
                try!(self.expect("Star"));
                self.parse_wildcard_index(CurrentNode)
            },
            _ => self.parse_multi_list()
        }
    }

    /// Examples: foo[*], foo[0], foo[:-1], etc.
    fn led_lbracket(&mut self, lhs: Ast) -> ParseResult {
        try!(self.expect("Number|Colon|Star"));
        match self.token {
            Token::Number {.. } | Token::Colon => {
                Ok(Ast::Subexpr(Box::new(lhs),
                                Box::new(try!(self.parse_array_index()))))
            },
            _ => self.parse_wildcard_index(lhs)
        }
    }

    fn nud_literal(&mut self, value: Json) -> ParseResult {
        self.advance();
        Ok(Ast::Literal(value))
    }

    /// Examples: "*" (e.g., "* | *" would be a pipe containing two nud stars)
    fn nud_star(&mut self) -> ParseResult {
        self.advance();
        self.parse_wildcard_values(Ast::CurrentNode)
    }

    /// Examples: "[]". Turns it into a led flatten (i.e., "@[]").
    fn nud_flatten(&mut self) -> ParseResult {
        self.led_flatten(Ast::CurrentNode)
    }

    /// Example "{foo: bar, baz: `12`}"
    fn nud_lbrace(&mut self) -> Result<Ast, ParseError> {
        let mut pairs = vec![];
        loop {
            // Skip the opening brace and any encountered commas.
            self.advance();
            // Requires at least on key value pair.
            pairs.push(try!(self.parse_kvp()));
            match self.token {
                // Terminal condition is the Rbrace token "}".
                Token::Rbrace => { self.advance(); break; },
                // Skip commas as they are used to delineate kvps
                Token::Comma => continue,
                _ => return Err(self.err("Expected '}' or ','"))
            }
        }
        Ok(Ast::MultiHash(pairs))
    }

    fn parse_kvp(&mut self) -> Result<KeyValuePair, ParseError> {
        match self.token.clone() {
            Token::Identifier { value, .. } => {
                try!(self.expect("Colon"));
                self.advance();
                Ok(KeyValuePair {
                    key: Ast::Literal(Json::String(value)),
                    value: try!(self.expr(0))
                })
            },
            _ => Err(self.err("Expected Identifier to start key value pair"))
        }
    }

    /// Creates a Projection AST node for a flatten token.
    fn led_flatten(&mut self, lhs: Ast) -> ParseResult {
        let rhs = try!(self.projection_rhs(Token::Flatten.lbp()));
        Ok(Ast::ArrayProjection(
            Box::new(Ast::Flatten(Box::new(lhs))),
            Box::new(rhs)
        ))
    }

    fn led_dot(&mut self, left: Ast) -> ParseResult {
        let rhs = try!(self.parse_dot(Token::Dot.lbp()));
        Ok(Ast::Subexpr(Box::new(left), Box::new(rhs)))
    }

    fn led_or(&mut self, left: Ast) -> ParseResult {
        self.advance();
        let rhs = try!(self.expr(Token::Or.lbp()));
        Ok(Ast::Or(Box::new(left), Box::new(rhs)))
    }

    fn led_lparen(&mut self, lhs: Ast) -> ParseResult {
        let fname: String;
        match lhs {
            Ast::Identifier(v) => fname = v,
            _ => return Err(self.err("Functions must be preceded by an identifier"))
        }
        self.advance();
        Ok(Ast::Function(fname, try!(self.parse_list(Token::Rparen))))
    }

    fn led_pipe(&mut self, left: Ast) -> ParseResult {
        self.advance();
        let rhs = try!(self.expr(Token::Pipe.lbp()));
        Ok(Ast::Subexpr(Box::new(left), Box::new(rhs)))
    }

    /// Parses the right hand side of a dot expression.
    fn parse_dot(&mut self, lbp: usize) -> ParseResult {
        try!(self.expect("Identifier|Star|Lbrace|Lbracket|Ampersand|Filter"));
        match self.token {
            Token::Lbracket => { self.advance(); self.parse_multi_list() },
            _ => self.expr(lbp)
        }
    }

    /// Parses the right hand side of a projection, using the given LBP to
    /// determine when to stop consuming tokens.
    fn projection_rhs(&mut self, lbp: usize) -> ParseResult {
        if self.token.lbp() < 10 {
            return Ok(CurrentNode);
        }
        match self.token {
            Token::Dot      => self.parse_dot(lbp),
            Token::Lbracket => self.expr(lbp),
            Token::Filter   => self.expr(lbp),
            _               => Err(self.token_err())
        }
    }

    /// Creates a projection for "[*]"
    fn parse_wildcard_index(&mut self, lhs: Ast) -> ParseResult {
        try!(self.expect("Rbracket"));
        let rhs = try!(self.projection_rhs(Token::Star.lbp()));
        Ok(Ast::ArrayProjection(Box::new(lhs), Box::new(rhs)))
    }

    /// Creates a projection for "*"
    fn parse_wildcard_values(&mut self, lhs: Ast) -> ParseResult {
        let rhs = try!(self.projection_rhs(Token::Star.lbp()));
        Ok(Ast::ObjectProjection(Box::new(lhs), Box::new(rhs)))
    }

    /// Parses [0], [::-1], [0:-1], [0:1], etc...
    fn parse_array_index(&mut self) -> ParseResult {
        let mut parts = [None, None, None];
        let mut pos = 0;
        loop {
            match self.token {
                Token::Colon => {
                    pos += 1;
                    if pos > 2 {
                        return Err(self.err("Too many colons in slice expr"));
                    }
                    try!(self.expect("Number|Colon|Rbracket"));
                },
                Token::Number { value, .. } => {
                    parts[pos] = Some(value);
                    try!(self.expect("Colon|Rbracket"));
                },
                Token::Rbracket => { self.advance(); break; },
                _ => return Err(self.token_err()),
            }
        }

        if pos == 0 {
            // No colons were found, so this is a simple index extraction.
            Ok(Ast::Index(parts[0].unwrap()))
        } else {
            // Sliced array from start (e.g., [2:])
            let lhs = Ast::Slice(parts[0], parts[1], parts[2]);
            let rhs = try!(self.projection_rhs(Token::Star.lbp()));
            Ok(Ast::ArrayProjection(Box::new(lhs), Box::new(rhs)))
        }
    }

    /// Parses multi-select lists (e.g., "[foo, bar, baz]")
    fn parse_multi_list(&mut self) -> ParseResult {
        Ok(Ast::MultiList(try!(self.parse_list(Token::Rbracket))))
    }

    /// Parse a comma separated list of expressions until a closing token or
    /// error. This function is used for functions and multi-list parsing.
    fn parse_list(&mut self, closing: Token) -> Result<Vec<Ast>, ParseError> {
        let mut nodes = vec![];
        loop {
            nodes.push(try!(self.expr(0)));
            if self.token == closing {
                break;
            } else if self.token == Token::Comma {
                self.advance();
            }
        }
        self.advance();
        Ok(nodes)
    }
}

#[cfg(test)]
mod test {
    extern crate rustc_serialize;
    use super::*;
    use ast::*;
    use self::rustc_serialize::json::{Json};

    #[test] fn indentifier_test() {
        assert_eq!(parse("foo").unwrap(),
                   Ast::Identifier("foo".to_string()));
    }

    #[test] fn current_node_test() {
        assert_eq!(parse("@").unwrap(), Ast::CurrentNode);
    }

    #[test] fn wildcard_values_test() {
        assert_eq!(parse("*").unwrap(),
                   Ast::ObjectProjection(Box::new(Ast::CurrentNode),
                                         Box::new(Ast::CurrentNode)));
    }

    #[test] fn dot_test() {
        assert_eq!(parse("@.b").unwrap(),
                  Ast::Subexpr(Box::new(Ast::CurrentNode),
                               bident(&"b")));
    }

    #[test] fn ensures_nud_token_is_valid_test() {
        let result = parse(",");
        assert!(result.is_err());
        assert!(result.err().unwrap().msg.contains("Unexpected token: Comma"));
    }

    #[test] fn multi_list_test() {
        let l = MultiList(vec![ident(&"a"), ident(&"b")]);
        assert_eq!(parse("[a, b]").unwrap(), l);
    }

    #[test] fn multi_list_unclosed() {
        let result = parse("[a, b");
        assert!(result.is_err());
        assert!(result.err().unwrap().msg.contains("Unexpected token"));
    }

    #[test] fn multi_list_unclosed_after_comma() {
        let result = parse("[a,");
        assert!(result.is_err());
        assert!(result.err().unwrap().msg.contains("Unexpected token"));
    }

    #[test] fn parse_error_includes_lexer_hints_test() {
        let result = parse(" \"foo");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().msg,
                   "Parse error at line 0, col 1; Unexpected token: Unknown\n \"foo\n ^\n\
                   Hint: Unclosed \" delimiter".to_string())
    }

    #[test] fn parse_error_on_column_zero_test() {
        let result = parse("]");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().msg,
                   "Parse error at line 0, col 0; Unexpected token: Rbracket\n\
                   ]\n^\n".to_string());
    }

    #[test] fn parse_error_injected_before_eof_test() {
        let result = parse("`\"foo\"` ||\n\n ]\n....]\n.");
        assert!(result.is_err());
        assert_eq!(result.err().unwrap().msg,
                   "Parse error at line 2, col 1; Unexpected token: Rbracket\n\
                   `\"foo\"` ||\n\n ]\n ^\n....]\n.\n".to_string());
    }

    #[test] fn can_parse_with_leading_whitespace_tokens_test() {
        assert_eq!(parse("\n\n`\"foo\"`").unwrap(),
                   Ast::Literal(Json::String("foo".to_string())))
    }

    #[test] fn multi_list_after_dot_test() {
        let l = Ast::MultiList(vec![ident(&"a"), ident(&"b")]);
        assert_eq!(parse("@.[a, b]").unwrap(),
                   Ast::Subexpr(Box::new(Ast::CurrentNode),
                                Box::new(l)));
    }

    #[test] fn parses_simple_index_extractions_test() {
        assert_eq!(parse("[0]").unwrap(), Ast::Index(0));
    }

    #[test] fn parses_single_element_slice_test() {
        assert_eq!(parse("[-1:]").unwrap(),
                   Ast::ArrayProjection(Box::new(Ast::Slice(Some(-1), None, None)),
                                        Box::new(Ast::CurrentNode)));
    }

    #[test] fn parses_double_element_slice_test() {
        assert_eq!(parse("[1:-1].a").unwrap(),
                   Ast::ArrayProjection(Box::new(Ast::Slice(Some(1), Some(-1), None)),
                                        bident(&"a")));
    }

    #[test] fn parses_revese_slice_test() {
        assert_eq!(parse("[::-1].a").unwrap(),
                   Ast::ArrayProjection(Box::new(Ast::Slice(None, None, Some(-1))),
                                        bident(&"a")));
    }

    #[test] fn parses_or_test() {
        let result = Ast::Or(bident(&"a"), bident(&"b"));
        assert_eq!(parse("a || b").unwrap(), result);
    }

    #[test] fn parses_pipe_test() {
        let result = Ast::Subexpr(bident(&"a"), bident(&"b"));
        assert_eq!(parse("a | b").unwrap(), result);
    }

    #[test] fn parses_literal_token_test() {
        assert_eq!(parse("`\"foo\"`").unwrap(),
                   Ast::Literal(Json::String("foo".to_string())))
    }

    #[test] fn parses_multi_hash() {
        let result = MultiHash(vec![
            KeyValuePair {
                key: Ast::Literal(Json::String("foo".to_string())),
                value: ident(&"bar")
            },
            KeyValuePair {
                key: Ast::Literal(Json::String("baz".to_string())),
                value: ident(&"bam")
            }
        ]);
        assert_eq!(parse("{foo: bar, baz: bam}").unwrap(), result);
    }

    #[test] fn parses_functions() {
        let r = Ast::Function("length".to_string(), vec![ident(&"a")]);
        assert_eq!(parse("length(a)").unwrap(), r);
    }

    #[test] fn parses_expref() {
        let result = Ast::Expref(bident(&"foo"));
        assert_eq!(parse("&foo").unwrap(), result);
    }

    fn bident(name: &str) -> Box<Ast> {
        Box::new(ident(name))
    }

    fn ident(name: &str) -> Ast {
        Ast::Identifier(name.to_string())
    }
}
