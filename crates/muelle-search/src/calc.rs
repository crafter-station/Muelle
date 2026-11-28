//! A small expression evaluator for the inline calculator.
//!
//! Supports `+ - * / ^ %`, parentheses, implicit multiplication (`2pi`,
//! `3(4+1)`), percentages (`15% of 80`, `200 + 10%`), the constants `pi` and
//! `e`, and the usual one-argument functions.

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Num(f64),
    Ident(String),
    Op(char),
}

/// Evaluates `input` if it looks like a calculation.
///
/// A bare number or word returns `None` so that ordinary searches are not
/// treated as maths. Prefix the input with `=` to force evaluation.
pub fn evaluate(input: &str) -> Option<f64> {
    let trimmed = input.trim();
    let (forced, body) = match trimmed.strip_prefix('=') {
        Some(rest) => (true, rest),
        None => (false, trimmed),
    };
    let tokens = tokenize(body)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        operations: 0,
        depth: 0,
    };
    let value = parser.expression()?.value;
    if parser.pos != parser.tokens.len() || !value.is_finite() {
        return None;
    }
    (forced || parser.operations > 0).then_some(value)
}

/// Formats a result for display, hiding floating point noise.
pub fn format(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let magnitude = value.abs();
    if !(1e-9..1e15).contains(&magnitude) {
        return format!("{value:e}");
    }
    // Keep 12 significant digits, then drop trailing zeros.
    let integer_digits = magnitude.log10().floor() as i32 + 1;
    let decimals = (12 - integer_digits).clamp(0, 20) as usize;
    let mut text = format!("{value:.decimals$}");
    if text.contains('.') {
        text.truncate(text.trim_end_matches('0').trim_end_matches('.').len());
    }
    text
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() || c == '.' {
            let mut number = String::new();
            while i < chars.len() {
                let d = chars[i];
                if d.is_ascii_digit() || d == '.' {
                    number.push(d);
                } else if d != ',' || !is_thousands_separator(&chars, i) {
                    break;
                }
                i += 1;
            }
            tokens.push(Token::Num(number.parse().ok()?));
        } else if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            tokens.push(Token::Ident(word.to_lowercase()));
        } else {
            let op = match c {
                '+' | '-' | '*' | '/' | '^' | '%' | '(' | ')' => c,
                '×' => '*',
                '÷' => '/',
                '−' => '-',
                _ => return None,
            };
            tokens.push(Token::Op(op));
            i += 1;
        }
    }
    (!tokens.is_empty()).then_some(tokens)
}

/// True when the comma at `i` is followed by exactly three digits.
fn is_thousands_separator(chars: &[char], i: usize) -> bool {
    let digit = |offset: usize| chars.get(i + offset).is_some_and(char::is_ascii_digit);
    digit(1) && digit(2) && digit(3) && !digit(4)
}

#[derive(Clone, Copy)]
struct Operand {
    value: f64,
    /// Set for a bare `n%`, which is relative when added or subtracted.
    percent: bool,
}

impl Operand {
    fn plain(value: f64) -> Self {
        Self {
            value,
            percent: false,
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    operations: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, op: char) -> bool {
        let found = self.peek() == Some(&Token::Op(op));
        if found {
            self.pos += 1;
        }
        found
    }

    fn starts_operand(token: Option<&Token>) -> bool {
        match token {
            Some(Token::Num(_)) | Some(Token::Op('(')) => true,
            Some(Token::Ident(name)) => name != "of",
            _ => false,
        }
    }

    fn expression(&mut self) -> Option<Operand> {
        let mut left = self.term()?;
        loop {
            let sign = if self.eat('+') {
                1.0
            } else if self.eat('-') {
                -1.0
            } else {
                return Some(left);
            };
            let right = self.term()?;
            self.operations += 1;
            let delta = if right.percent && !left.percent {
                left.value * right.value
            } else {
                right.value
            };
            left = Operand::plain(left.value + sign * delta);
        }
    }

    fn term(&mut self) -> Option<Operand> {
        let mut left = self.unary()?;
        loop {
            let value = if self.eat('*') {
                left.value * self.unary()?.value
            } else if self.eat('/') {
                left.value / self.unary()?.value
            } else if self.eat('%') {
                // `postfix` consumed every percent sign, so this is modulo.
                left.value % self.unary()?.value
            } else if matches!(self.peek(), Some(Token::Ident(name)) if name == "of") {
                self.pos += 1;
                left.value * self.unary()?.value
            } else if matches!(self.peek(), Some(Token::Ident(_)) | Some(Token::Op('('))) {
                left.value * self.unary()?.value
            } else {
                return Some(left);
            };
            self.operations += 1;
            left = Operand::plain(value);
        }
    }

    fn unary(&mut self) -> Option<Operand> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let result = if self.eat('-') {
            let inner = self.unary()?;
            Some(Operand {
                value: -inner.value,
                ..inner
            })
        } else if self.eat('+') {
            self.unary()
        } else {
            self.power()
        };
        self.depth -= 1;
        result
    }

    fn power(&mut self) -> Option<Operand> {
        let base = self.postfix()?;
        if self.eat('^') {
            let exponent = self.unary()?;
            self.operations += 1;
            return Some(Operand::plain(base.value.powf(exponent.value)));
        }
        Some(base)
    }

    fn postfix(&mut self) -> Option<Operand> {
        let mut operand = self.primary()?;
        // `%` is a percentage unless another operand follows it.
        while self.peek() == Some(&Token::Op('%'))
            && !Self::starts_operand(self.tokens.get(self.pos + 1))
        {
            self.pos += 1;
            self.operations += 1;
            operand = Operand {
                value: operand.value / 100.0,
                percent: true,
            };
        }
        Some(operand)
    }

    fn primary(&mut self) -> Option<Operand> {
        let token = self.peek()?.clone();
        self.pos += 1;
        match token {
            Token::Num(n) => Some(Operand::plain(n)),
            Token::Op('(') => {
                let inner = self.expression()?;
                // A missing closing bracket is fine while still typing.
                if !self.eat(')') && self.pos != self.tokens.len() {
                    return None;
                }
                Some(Operand::plain(inner.value))
            }
            Token::Ident(name) => match name.as_str() {
                "pi" => Some(Operand::plain(std::f64::consts::PI)),
                "e" => Some(Operand::plain(std::f64::consts::E)),
                _ => {
                    let function: fn(f64) -> f64 = match name.as_str() {
                        "sqrt" => f64::sqrt,
                        "cbrt" => f64::cbrt,
                        "abs" => f64::abs,
                        "round" => f64::round,
                        "floor" => f64::floor,
                        "ceil" => f64::ceil,
                        "sin" => f64::sin,
                        "cos" => f64::cos,
                        "tan" => f64::tan,
                        "asin" => f64::asin,
                        "acos" => f64::acos,
                        "atan" => f64::atan,
                        "ln" => f64::ln,
                        "log" => f64::log10,
                        "exp" => f64::exp,
                        _ => return None,
                    };
                    let argument = self.unary()?;
                    self.operations += 1;
                    Some(Operand::plain(function(argument.value)))
                }
            },
            Token::Op(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(input: &str) -> Option<String> {
        evaluate(input).map(format)
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(result("2+3*4").as_deref(), Some("14"));
        assert_eq!(result("(2+3)*4").as_deref(), Some("20"));
        assert_eq!(result("2^3^2").as_deref(), Some("512"));
        assert_eq!(result("-2^2").as_deref(), Some("-4"));
        assert_eq!(result("10 / 4").as_deref(), Some("2.5"));
        assert_eq!(result("0.1+0.2").as_deref(), Some("0.3"));
        assert_eq!(result("1,000 * 3").as_deref(), Some("3000"));
        assert_eq!(result("6×7").as_deref(), Some("42"));
    }

    #[test]
    fn percentages_and_modulo() {
        assert_eq!(result("15% of 80").as_deref(), Some("12"));
        assert_eq!(result("200 + 10%").as_deref(), Some("220"));
        assert_eq!(result("200 - 10%").as_deref(), Some("180"));
        assert_eq!(result("10 % 3").as_deref(), Some("1"));
        assert_eq!(result("50%").as_deref(), Some("0.5"));
    }

    #[test]
    fn functions_and_constants() {
        assert_eq!(result("sqrt(81)").as_deref(), Some("9"));
        assert_eq!(result("sqrt 16 + 1").as_deref(), Some("5"));
        assert_eq!(result("2pi").as_deref(), Some("6.28318530718"));
        assert_eq!(result("3(4+1").as_deref(), Some("15"));
        assert_eq!(result("=pi").as_deref(), Some("3.14159265359"));
    }

    #[test]
    fn ordinary_searches_are_ignored() {
        for input in [
            "", "42", "-5", "e", "pi", "notepad", "7-zip", "c++", "1/0", "2+", "1,2+3",
        ] {
            assert_eq!(evaluate(input), None, "{input:?}");
        }
    }

    #[test]
    fn deeply_nested_input_is_rejected() {
        let input = format!("{}1+1", "-".repeat(500));
        assert_eq!(evaluate(&input), None);
    }
}
