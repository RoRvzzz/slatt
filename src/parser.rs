use crate::ir::{Block, Comparison, Const, Inst, Op, Operand, Proto, Reg};
use crate::rules::{
    Arm, Guard, MapArg, Mapping, Pass, PatternArg, PatternInst, Rule, Script, Step, Term,
};
use std::collections::{BTreeMap, BTreeSet};
use std::iter::Peekable;
use std::str::Chars;

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Number(String),
    String(Vec<u8>),
    Symbol(char),
    Arrow,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    End,
}

struct Located {
    token: Token,
    line: usize,
    column: usize,
}

struct Lexer<'a> {
    chars: Peekable<Chars<'a>>,
    line: usize,
    column: usize,
}

impl Lexer<'_> {
    fn next(&mut self) -> Option<char> {
        let ch = self.chars.next()?;
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn take(&mut self, expected: char) -> bool {
        if self.chars.peek() == Some(&expected) {
            self.next();
            true
        } else {
            false
        }
    }

    fn scan(&mut self, condition: impl Fn(char) -> bool) -> String {
        let mut out = String::new();
        while self.chars.peek().is_some_and(|c| condition(*c)) {
            out.push(self.next().unwrap());
        }
        out
    }

    fn number(&mut self, first: char) -> String {
        let mut text = String::from(first);
        text.push_str(&self.scan(|c| c.is_ascii_digit()));
        if self.take('.') {
            text.push('.');
            text.push_str(&self.scan(|c| c.is_ascii_digit()));
        }
        if self.take('e') || self.take('E') {
            text.push('e');
            if self.take('-') {
                text.push('-');
            } else if self.take('+') {
                text.push('+');
            }
            text.push_str(&self.scan(|c| c.is_ascii_digit()));
        }
        text
    }

    fn string(&mut self) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        loop {
            let ch = match self.next() {
                Some('"') => return Ok(out),
                Some('\\') => match self.next() {
                    Some('"') => '"',
                    Some('\\') => '\\',
                    Some('n') => '\n',
                    Some('r') => '\r',
                    Some('t') => '\t',
                    Some('0') => '\0',
                    Some('x') => {
                        let high = self
                            .next()
                            .and_then(|c| c.to_digit(16))
                            .ok_or("invalid byte escape")?;
                        let low = self
                            .next()
                            .and_then(|c| c.to_digit(16))
                            .ok_or("invalid byte escape")?;
                        out.push((high * 16 + low) as u8);
                        continue;
                    }
                    Some('u') if self.take('{') => {
                        let digits = self.scan(|ch| ch.is_ascii_hexdigit());
                        if !self.take('}') {
                            return Err("expected } after the Unicode escape".into());
                        }
                        u32::from_str_radix(&digits, 16)
                            .ok()
                            .and_then(char::from_u32)
                            .ok_or("invalid Unicode escape")?
                    }
                    _ => return Err("invalid string escape".into()),
                },
                Some(ch) if !ch.is_control() => ch,
                _ => return Err("unterminated string literal".into()),
            };
            let mut utf8 = [0; 4];
            out.extend_from_slice(ch.encode_utf8(&mut utf8).as_bytes());
        }
    }

    fn lex(mut self) -> Result<Vec<Located>, String> {
        let mut tokens = Vec::new();
        loop {
            let (line, column) = (self.line, self.column);
            let token = match self.next() {
                None => Token::End,
                Some(ch) if ch.is_whitespace() => continue,
                Some('/') if self.take('/') => {
                    self.scan(|ch| ch != '\n');
                    continue;
                }
                Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {
                    let mut word = String::from(ch);
                    word.push_str(&self.scan(|ch| ch.is_ascii_alphanumeric() || ch == '_'));
                    Token::Word(word)
                }
                Some(ch) if ch.is_ascii_digit() || ch == '-' => Token::Number(self.number(ch)),
                Some('"') => Token::String(
                    self.string()
                        .map_err(|msg| format!("{line}:{column}: {msg}"))?,
                ),
                Some('=') if self.take('>') => Token::Arrow,
                Some('=') if self.take('=') => Token::Equal,
                Some('!') if self.take('=') => Token::NotEqual,
                Some('<') if self.take('=') => Token::LessEqual,
                Some('>') if self.take('=') => Token::GreaterEqual,
                Some('<') => Token::Less,
                Some('>') => Token::Greater,
                Some(ch) if "()[]{}@,:;$".contains(ch) => Token::Symbol(ch),
                Some(ch) => return Err(format!("{line}:{column}: unexpected character {ch:?}")),
            };
            let end = token == Token::End;
            tokens.push(Located {
                token,
                line,
                column,
            });
            if end {
                return Ok(tokens);
            }
        }
    }
}

struct Parser {
    tokens: Vec<Located>,
    position: usize,
}

impl Parser {
    fn new(source: &str) -> Result<Self, String> {
        let source = source.strip_prefix('\u{feff}').unwrap_or(source);
        let tokens = Lexer {
            chars: source.chars().peekable(),
            line: 1,
            column: 1,
        }
        .lex()?;
        Ok(Self {
            tokens,
            position: 0,
        })
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.position].token
    }

    fn error(&self, message: impl std::fmt::Display) -> String {
        let token = &self.tokens[self.position];
        format!("{}:{}: {message}", token.line, token.column)
    }

    fn take(&mut self, token: Token) -> bool {
        if self.peek() == &token {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, keyword: &str) -> bool {
        if matches!(self.peek(), Token::Word(word) if word == keyword) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn expect_word(&mut self, keyword: &str) -> Result<(), String> {
        self.word(keyword)
            .then_some(())
            .ok_or_else(|| self.error(format!("expected {keyword}")))
    }

    fn expect(&mut self, ch: char) -> Result<(), String> {
        self.take(Token::Symbol(ch))
            .then_some(())
            .ok_or_else(|| self.error(format!("expected {ch}")))
    }

    fn arrow(&mut self) -> Result<(), String> {
        self.take(Token::Arrow)
            .then_some(())
            .ok_or_else(|| self.error("expected =>"))
    }

    fn ident(&mut self) -> Result<String, String> {
        let Token::Word(word) = self.peek() else {
            return Err(self.error("expected an identifier"));
        };
        let word = word.clone();
        self.position += 1;
        Ok(word)
    }

    fn unsigned<T: std::str::FromStr>(&mut self) -> Result<T, String> {
        let Token::Number(text) = self.peek() else {
            return Err(self.error("expected an unsigned integer"));
        };
        let value = text
            .parse()
            .map_err(|_| self.error("invalid unsigned integer"))?;
        self.position += 1;
        Ok(value)
    }

    fn literal(&mut self) -> Result<Const, String> {
        if self.word("bits") {
            self.expect('(')?;
            let Token::String(text) = self.peek() else {
                return Err(self.error("expected 16 hexadecimal digits"));
            };
            let bits = std::str::from_utf8(text)
                .ok()
                .filter(|s| s.len() == 16 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                .and_then(|s| u64::from_str_radix(s, 16).ok())
                .ok_or_else(|| self.error("expected 16 hexadecimal digits"))?;
            self.position += 1;
            self.expect(')')?;
            return Ok(Const::Number(f64::from_bits(bits)));
        }
        let value = match self.peek() {
            Token::Number(text) => {
                let number = text
                    .parse::<f64>()
                    .ok()
                    .filter(|number| number.is_finite())
                    .ok_or_else(|| self.error("expected a finite number"))?;
                Const::Number(number)
            }
            Token::String(text) => Const::String(text.clone()),
            Token::Word(text) if text == "nil" => Const::Nil,
            Token::Word(text) if text == "true" || text == "false" => Const::Bool(text == "true"),
            _ => return Err(self.error("expected a constant literal")),
        };
        self.position += 1;
        Ok(value)
    }

    fn operand(&mut self) -> Result<Operand, String> {
        if self.take(Token::Symbol('@')) {
            return Ok(Operand::Block(self.ident()?));
        }
        if let Token::Word(name) = self.peek()
            && let Some(number) = name.strip_prefix('r')
        {
            let number = number
                .parse()
                .map_err(|_| self.error("expected a register r0 through r65535"))?;
            self.position += 1;
            return Ok(Operand::Reg(Reg(number)));
        }
        Ok(Operand::Const(self.literal()?))
    }

    fn op(&mut self) -> Result<Op, String> {
        let Token::Word(name) = self.peek() else {
            return Err(self.error("expected an opcode"));
        };
        let op = Op::parse(name).ok_or_else(|| self.error(format!("unknown opcode {name}")))?;
        self.position += 1;
        Ok(op)
    }

    fn list<T>(
        &mut self,
        start: char,
        end: char,
        item: fn(&mut Self) -> Result<T, String>,
    ) -> Result<Vec<T>, String> {
        self.expect(start)?;
        let mut items = Vec::new();
        if !self.take(Token::Symbol(end)) {
            loop {
                items.push(item(self)?);
                if self.take(Token::Symbol(end)) {
                    break;
                }
                self.expect(',')?;
            }
        }
        Ok(items)
    }

    fn pattern(&mut self) -> Result<PatternInst, String> {
        let op = self.op()?;
        let args = self.list('(', ')', |p| {
            if p.take(Token::Symbol('$')) {
                Ok(PatternArg::Bind(p.ident()?))
            } else {
                Ok(PatternArg::Literal(p.operand()?))
            }
        })?;
        Ok(PatternInst { op, args })
    }

    fn term(&mut self) -> Result<Term, String> {
        if self.word("const") {
            self.expect('(')?;
            self.expect('$')?;
            let name = self.ident()?;
            self.expect(')')?;
            return Ok(Term::Bound(name));
        }
        Ok(Term::Literal(self.literal()?))
    }

    fn guard(&mut self) -> Result<Guard, String> {
        if self.word("known") {
            self.expect('(')?;
            self.expect('$')?;
            let term = Term::Bound(self.ident()?);
            self.expect(')')?;
            return Ok(Guard::Known(term));
        }
        let left = self.term()?;
        let comparison = match self.peek() {
            Token::Equal => Comparison::Eq,
            Token::NotEqual => Comparison::Ne,
            Token::Less => Comparison::Lt,
            Token::LessEqual => Comparison::Le,
            Token::Greater => Comparison::Gt,
            Token::GreaterEqual => Comparison::Ge,
            _ => return Err(self.error("expected a comparison operator")),
        };
        self.position += 1;
        Ok(Guard::Compare {
            left,
            right: self.term()?,
            comparison,
        })
    }

    fn rule(&mut self) -> Result<Rule, String> {
        let pattern = self.list('[', ']', Self::pattern)?;
        let mut arms = vec![self.arm()?];
        while self.take(Token::Symbol(',')) {
            arms.push(self.arm()?);
        }
        let rule = Rule { pattern, arms };
        rule.check().map_err(|error| self.error(error))?;
        self.expect(';')?;
        Ok(rule)
    }

    fn arm(&mut self) -> Result<Arm, String> {
        let mut guards = Vec::new();
        if self.word("where") {
            guards.push(self.guard()?);
            while self.word("and") {
                guards.push(self.guard()?);
            }
        }
        self.arrow()?;
        let replacement = self.list('[', ']', Self::pattern)?;
        Ok(Arm {
            guards,
            replacement,
        })
    }

    fn pass(&mut self, name: String) -> Result<Pass, String> {
        let mut steps = Vec::new();
        self.expect('{')?;
        while !self.take(Token::Symbol('}')) {
            if self.word("fold") {
                self.expect_word("constants")?;
                self.expect(';')?;
                steps.push(Step::Fold);
            } else if self.word("match") {
                steps.push(Step::Rewrite(self.rule()?));
            } else if self.word("prune") {
                self.expect_word("unreachable")?;
                self.expect(';')?;
                steps.push(Step::Prune);
            } else if self.word("dce") {
                self.expect(';')?;
                steps.push(Step::Dce);
            } else {
                return Err(
                    self.error("expected match, fold constants, prune unreachable, dce, or }")
                );
            }
        }
        Ok(Pass { name, steps })
    }

    fn map(&mut self) -> Result<BTreeMap<u32, Mapping>, String> {
        let mut rules = BTreeMap::new();
        self.expect('{')?;
        while !self.take(Token::Symbol('}')) {
            let opcode = self.unsigned()?;
            self.arrow()?;
            let op = self.op()?;
            let args = self.list('(', ')', |p| {
                if p.take(Token::Symbol('$')) {
                    Ok(MapArg::Slot(p.unsigned()?))
                } else {
                    Ok(MapArg::Literal(p.operand()?))
                }
            })?;
            let rule = Mapping { op, args };
            rule.check().map_err(|error| self.error(error))?;
            if rules.insert(opcode, rule).is_some() {
                return Err(self.error(format!("duplicate raw opcode {opcode}")));
            }
            self.expect(';')?;
        }
        Ok(rules)
    }

    fn proto(
        &mut self,
        name: String,
        mapping: Option<&BTreeMap<u32, Mapping>>,
    ) -> Result<Proto, String> {
        let mut blocks = Vec::new();
        self.expect('{')?;
        while !self.take(Token::Symbol('}')) {
            self.expect('@')?;
            let name = self.ident()?;
            self.expect(':')?;
            let mut instructions = Vec::new();
            while !matches!(self.peek(), Token::Symbol('@' | '}')) {
                let inst = if let Some(mapping) = mapping {
                    let opcode = self.unsigned()?;
                    let rule = mapping
                        .get(&opcode)
                        .ok_or_else(|| self.error(format!("unmapped raw opcode {opcode}")))?;
                    let args = self.list('(', ')', Self::operand)?;
                    rule.lift(&args)
                } else {
                    let op = self.op()?;
                    let args = self.list('(', ')', Self::operand)?;
                    Inst::new(op, args)
                }
                .map_err(|error| self.error(error))?;
                self.expect(';')?;
                instructions.push(inst);
            }
            blocks.push(Block { name, instructions });
        }
        let proto = Proto { name, blocks };
        proto.verify().map_err(|error| self.error(error))?;
        Ok(proto)
    }
}

pub fn parse_script(source: &str) -> Result<Script, String> {
    let mut p = Parser::new(source)?;
    let mut names = BTreeSet::new();
    let mut script = Script::default();
    while p.peek() != &Token::End {
        if p.word("map") {
            let name = p.ident()?;
            if script.maps.contains_key(&name) {
                return Err(p.error(format!("duplicate map {name}")));
            }
            script.maps.insert(name, p.map()?);
        } else if p.word("pass") {
            let name = p.ident()?;
            if !names.insert(name.clone()) {
                return Err(p.error(format!("duplicate pass {name}")));
            }
            script.passes.push(p.pass(name)?);
        } else {
            return Err(p.error("expected map or pass"));
        }
    }
    Ok(script)
}

pub fn parse_ir(source: &str, script: &Script) -> Result<Vec<Proto>, String> {
    let mut p = Parser::new(source)?;
    let mut names = BTreeSet::new();
    let mut protos = Vec::new();
    while p.peek() != &Token::End {
        let raw = p.word("raw");
        p.expect_word("proto")?;
        let name = p.ident()?;
        if !names.insert(name.clone()) {
            return Err(p.error(format!("duplicate prototype {name}")));
        }
        let mapping = if raw {
            p.expect_word("using")?;
            let map = p.ident()?;
            Some(
                script
                    .maps
                    .get(&map)
                    .ok_or_else(|| p.error(format!("unknown opcode map {map}")))?,
            )
        } else {
            None
        };
        protos.push(p.proto(name, mapping)?);
    }
    if protos.is_empty() {
        return Err(p.error("expected at least one prototype"));
    }
    Ok(protos)
}
