//! Criterion evaluation (spec 5.8.11): simple conditions, regex, JSONPath,
//! XPath. Any evaluation error is a fail (5.8.11.4.5), never a pass.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::expr::{self, Scope};
use crate::model::{Criterion, CriterionType, ExpressionType, SelectorKind};

/// Checks one criterion; an evaluation error counts as "no".
///
/// An evaluation error counts as a failure, not a pass.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::condition;
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::model::Criterion;
/// use arazzo_jrpc::transport::Exchange;
///
/// let mut scope = Scope::default();
/// scope.exchange = Some(Exchange { status: 200, ..Default::default() });
/// assert!(condition::passes(&Criterion::simple("$statusCode == 200"), &scope));
/// assert!(!condition::passes(&Criterion::simple("$statusCode =="), &scope));
/// ```
pub fn passes(c: &Criterion, scope: &Scope) -> bool {
    evaluate(c, scope).unwrap_or(false)
}

/// Checks a criteria list as a logical AND (5.8.11.4.6).
///
/// An empty list passes.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::condition;
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::model::Criterion;
/// use arazzo_jrpc::transport::Exchange;
///
/// let mut scope = Scope::default();
/// scope.exchange = Some(Exchange { status: 200, ..Default::default() });
/// let both = [Criterion::simple("$statusCode == 200"), Criterion::simple("$statusCode != 404")];
/// assert!(condition::all(&both, &scope));
/// assert!(!condition::all(&[Criterion::simple("$statusCode == 201")], &scope));
/// assert!(condition::all(&[], &scope));
/// ```
pub fn all(criteria: &[Criterion], scope: &Scope) -> bool {
    criteria.iter().all(|c| passes(c, scope))
}

/// Finds the first criterion that does not hold, with why.
///
/// `None` when all pass; otherwise the failed condition or the evaluation error.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::condition;
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::model::Criterion;
/// use arazzo_jrpc::transport::Exchange;
///
/// let mut scope = Scope::default();
/// scope.exchange = Some(Exchange { status: 500, ..Default::default() });
/// let why = condition::first_failure(&[Criterion::simple("$statusCode == 200")], &scope);
/// assert_eq!(why.as_deref(), Some("not satisfied: $statusCode == 200"));
/// assert_eq!(condition::first_failure(&[], &scope), None);
/// ```
pub fn first_failure(criteria: &[Criterion], scope: &Scope) -> Option<String> {
    criteria.iter().find_map(|c| match evaluate(c, scope) {
        Ok(true) => None,
        Ok(false) => Some(format!("not satisfied: {}", c.condition)),
        Err(e) => Some(format!("{}: {e}", c.condition)),
    })
}

/// Evaluates one criterion.
///
/// A typed criterion whose context is `null` evaluates to `false`.
///
/// # Errors
///
/// The criterion cannot be evaluated: a syntax error, a missing reference, an
/// invalid regex, JSONPath or XPath, or XPath over a non-XML body. Callers treat
/// that as a failed criterion.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::condition;
/// use arazzo_jrpc::expr::Scope;
/// use arazzo_jrpc::model::Criterion;
/// use arazzo_jrpc::transport::Exchange;
/// use serde_json::json;
///
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let mut scope = Scope::default();
/// scope.exchange = Some(Exchange { status: 200, response_body: json!({"pets": [1]}), ..Default::default() });
/// let regex: Criterion = serde_json::from_value(json!({"context": "$statusCode", "condition": "^2", "type": "regex"}))?;
/// assert_eq!(condition::evaluate(&regex, &scope)?, true);
/// let path: Criterion = serde_json::from_value(json!({"context": "$response.body", "condition": "$.pets[*]", "type": "jsonpath"}))?;
/// assert_eq!(condition::evaluate(&path, &scope)?, true);
/// assert!(condition::evaluate(&Criterion::simple("(1 == 1"), &scope).is_err());
/// # Ok(())
/// # }
/// ```
pub fn evaluate(c: &Criterion, scope: &Scope) -> Result<bool> {
    let kind = match &c.kind {
        CriterionType::Simple => return simple::eval(&c.condition, scope),
        other => other,
    };
    let context = c.context.as_deref().ok_or("a criterion with type requires context")?;
    let target = expr::evaluate(context, scope)?;
    if target.is_null() {
        return Ok(false);
    }
    // Embedded {$…} are substituted before the expression is parsed (5.8.11.3).
    let condition = embed_expressions(&c.condition, scope)?;
    match kind {
        CriterionType::Regex => regex_matches(&condition, &target),
        CriterionType::Expression(t) => expression_matches(t, &condition, &target),
        CriterionType::Simple => unreachable!("simple is handled above"),
    }
}

/// Runs an RFC 9535 JSONPath query; the Goessner draft is served by the
/// same engine, whose syntax is a superset of what the draft's examples use.
///
/// Matches form a nodelist, e.g. `["A", "B"]` for `$[*].sku`; empty when
/// nothing matched.
///
/// # Errors
///
/// The query is not valid JSONPath.
///
/// # Examples
///
/// ```
/// use arazzo_jrpc::condition::jsonpath;
/// use serde_json::json;
///
/// # fn main() -> arazzo_jrpc::Result<()> {
/// let items = json!([{"sku": "A"}, {"sku": "B"}]);
/// assert_eq!(jsonpath("$[*].sku", &items)?, vec![json!("A"), json!("B")]);
/// assert!(jsonpath("$[?@.sku == 'Z']", &items)?.is_empty());
/// assert!(jsonpath("$[[", &items).is_err());
/// # Ok(())
/// # }
/// ```
pub fn jsonpath(query: &str, target: &Value) -> Result<Vec<Value>> {
    let path = serde_json_path::JsonPath::parse(query).map_err(|e| Error(format!("JSONPath {query:?}: {e}")))?;
    Ok(path.query(target).all().into_iter().cloned().collect())
}

/// `{$…}` parts replaced by their string form; the rest untouched (JSONPath
/// and XPath use braces and `$` themselves).
fn embed_expressions(text: &str, scope: &Scope) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find("{$") {
        let (e, tail) = expr::parse_prefix(&rest[open + 1..])?;
        let tail = tail.strip_prefix('}').ok_or_else(|| Error(format!("{text}: unclosed {{")))?;
        out.push_str(&rest[..open]);
        out.push_str(&expr::embed(&expr::eval(&e, scope).map_err(Error)?));
        rest = tail;
    }
    out.push_str(rest);
    Ok(out)
}

/// A regex criterion: the pattern against the context's string form.
fn regex_matches(pattern: &str, target: &Value) -> Result<bool> {
    let re = fancy_regex::Regex::new(pattern).map_err(|e| Error(format!("regex {pattern:?}: {e}")))?;
    re.is_match(&expr::embed(target)).map_err(|e| Error(format!("regex {pattern:?}: {e}")))
}

/// A JSONPath / JSON Pointer / XPath criterion: does it select anything?
fn expression_matches(t: &ExpressionType, condition: &str, target: &Value) -> Result<bool> {
    match t.kind {
        SelectorKind::Jsonpath => Ok(!jsonpath(condition, target)?.is_empty()),
        SelectorKind::Jsonpointer => Ok(target.pointer(condition).is_some()),
        SelectorKind::Xpath => xpath_truthy(target, condition, t.version()),
    }
}

#[cfg(feature = "xpath")]
fn xpath_truthy(target: &Value, expr: &str, version: &str) -> Result<bool> {
    let xml = target.as_str().ok_or("XPath applies only to XML (a string)")?;
    crate::xpath::truthy(xml, expr, version).map_err(Error)
}

#[cfg(not(feature = "xpath"))]
fn xpath_truthy(_: &Value, _: &str, _: &str) -> Result<bool> {
    Err(Error("XPath unavailable: crate built without the xpath feature".into()))
}

/// Simple conditions: literals, runtime expressions and the operators of
/// 5.8.11.2 with the conversions of 5.8.11.4.1.
mod simple {
    use std::cmp::Ordering;

    use serde_json::Value;

    use crate::error::{Error, Result};
    use crate::expr::{self, Expr, Scope};

    /// Evaluates a simple condition; only `true` passes (null and every
    /// non-boolean value fail).
    ///
    /// # Errors
    ///
    /// The condition has a syntax error or references something the scope does not
    /// have, e.g. `$statusCode ==: expected a value`.
    pub fn eval(text: &str, scope: &Scope) -> Result<bool> {
        let toks = lex(text)?;
        let mut p = Parser { toks: &toks, pos: 0, scope, text };
        let v = p.or()?;
        if p.pos != toks.len() {
            return Err(p.err("trailing input after the condition"));
        }
        Ok(v == true)
    }

    // ------------------------------------------------------------ lexer

    #[derive(Debug, Clone, PartialEq)]
    enum Tok {
        Expr(Expr),
        Lit(Value),
        Op(&'static str),
        LParen,
        RParen,
    }

    /// Two-char operators first, so `<=` is not read as `<`.
    const OPS: &[&str] = &["==", "!=", "<=", ">=", "&&", "||", "<", ">", "!"];

    fn lex(text: &str) -> Result<Vec<Tok>> {
        let mut out = Vec::new();
        let mut s = text.trim_start();
        while !s.is_empty() {
            let (tok, rest) = token(s, text)?;
            out.push(tok);
            s = rest.trim_start();
        }
        Ok(out)
    }

    /// The token at the start of non-empty `s`, and what follows it.
    fn token<'s>(s: &'s str, text: &str) -> Result<(Tok, &'s str)> {
        let c = s.chars().next().expect("lex never calls token on an empty string");
        match c {
            '$' => expr::parse_prefix(s).map(|(e, rest)| (Tok::Expr(e), rest)),
            '\'' => string(&s[1..])
                .map(|(lit, rest)| (Tok::Lit(Value::String(lit)), rest))
                .ok_or_else(|| Error(format!("{text}: unclosed string"))),
            '(' => Ok((Tok::LParen, &s[1..])),
            ')' => Ok((Tok::RParen, &s[1..])),
            _ => match OPS.iter().find(|op| s.starts_with(**op)) {
                Some(op) => operator(op, s, text),
                None if c.is_ascii_digit() || c == '-' || c == '.' => number(s, text),
                None => word(s, text),
            },
        }
    }

    /// `op` at the start of `s`; a trailing `=` after a two-char operator
    /// (`<==`, `===`) is an error, not two tokens.
    fn operator<'s>(op: &'static str, s: &'s str, text: &str) -> Result<(Tok, &'s str)> {
        if s[op.len()..].starts_with('=') && op.len() == 2 {
            return Err(Error(format!("{text}: unknown operator {op}=")));
        }
        Ok((Tok::Op(op), &s[op.len()..]))
    }

    /// A JSON number: `200`, `-1.5`, `1e3`.
    fn number<'s>(s: &'s str, text: &str) -> Result<(Tok, &'s str)> {
        let end = s.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))).unwrap_or(s.len());
        let n: Value = serde_json::from_str(&s[..end]).map_err(|_| Error(format!("{text}: bad number {:?}", &s[..end])))?;
        Ok((Tok::Lit(n), &s[end..]))
    }

    /// `true`, `false` or `null`; any other word is an error.
    fn word<'s>(s: &'s str, text: &str) -> Result<(Tok, &'s str)> {
        let end = s.find(|c: char| !c.is_ascii_alphanumeric()).unwrap_or(s.len());
        let lit = match &s[..end] {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            "null" => Value::Null,
            other => return Err(Error(format!("{text}: unexpected {other:?}"))),
        };
        Ok((Tok::Lit(lit), &s[end..]))
    }

    /// `'…'` with `''` for a quote; `s` starts after the opening quote.
    fn string(s: &str) -> Option<(String, &str)> {
        let mut out = String::new();
        let mut chars = s.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if matches!(chars.peek(), Some((_, '\''))) {
                    out.push('\'');
                    chars.next();
                } else {
                    return Some((out, &s[i + 1..]));
                }
            } else {
                out.push(c);
            }
        }
        None
    }

    // ------------------------------------------------------------ parser

    /// Precedence climbing: `||` < `&&` < comparison < `!` < primary;
    /// evaluates while parsing.
    struct Parser<'a> {
        toks: &'a [Tok],
        pos: usize,
        scope: &'a Scope,
        text: &'a str,
    }

    impl Parser<'_> {
        fn peek(&self) -> Option<&Tok> {
            self.toks.get(self.pos)
        }

        /// Consumes the next token when it equals `tok`.
        fn eat(&mut self, tok: &Tok) -> bool {
            let hit = self.peek() == Some(tok);
            if hit {
                self.pos += 1;
            }
            hit
        }

        fn err(&self, m: &str) -> Error {
            Error(format!("{}: {m}", self.text))
        }

        fn or(&mut self) -> Result<Value> {
            let mut v = self.and()?;
            while self.eat(&Tok::Op("||")) {
                let r = self.and()?;
                v = Value::Bool(v == true || r == true);
            }
            Ok(v)
        }

        fn and(&mut self) -> Result<Value> {
            let mut v = self.cmp()?;
            while self.eat(&Tok::Op("&&")) {
                let r = self.cmp()?;
                v = Value::Bool(v == true && r == true);
            }
            Ok(v)
        }

        /// At most one comparison: in `a == b == c` the second `==` is left over.
        fn cmp(&mut self) -> Result<Value> {
            let l = self.unary()?;
            let Some(Tok::Op(op)) = self.peek().cloned() else { return Ok(l) };
            if !matches!(op, "==" | "!=" | "<" | "<=" | ">" | ">=") {
                return Ok(l);
            }
            self.pos += 1;
            let r = self.unary()?;
            Ok(Value::Bool(compare(&l, op, &r)))
        }

        fn unary(&mut self) -> Result<Value> {
            if !self.eat(&Tok::Op("!")) {
                return self.primary();
            }
            match self.unary()? {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => Err(self.err(&format!("! applies to boolean, not to {other}"))),
            }
        }

        fn primary(&mut self) -> Result<Value> {
            let tok = self.peek().cloned().ok_or_else(|| self.err("expected a value"))?;
            self.pos += 1;
            match tok {
                Tok::Lit(v) => Ok(v),
                Tok::Expr(e) => expr::eval(&e, self.scope).map_err(|m| self.err(&m)),
                Tok::LParen => {
                    let v = self.or()?;
                    if !self.eat(&Tok::RParen) {
                        return Err(self.err("missing closing )"));
                    }
                    Ok(v)
                }
                other => Err(self.err(&format!("unexpected {other:?}"))),
            }
        }
    }

    // ------------------------------------------------------------ comparison

    /// 5.8.11.4.1: case-insensitive strings, numeric strings coerced by
    /// numeric comparisons, null equal only to null (`!=` against null is
    /// true for anything else, ordering against null is false).
    fn compare(l: &Value, op: &str, r: &Value) -> bool {
        if l.is_null() || r.is_null() {
            return match op {
                "==" => l.is_null() && r.is_null(),
                "!=" => !(l.is_null() && r.is_null()),
                _ => false,
            };
        }
        let order = match (as_number(l), as_number(r)) {
            (Some(a), Some(b)) if l.is_number() || r.is_number() => a.partial_cmp(&b),
            _ => match (l, r) {
                (Value::String(a), Value::String(b)) => Some(a.to_lowercase().cmp(&b.to_lowercase())),
                (a, b) if a == b => Some(Ordering::Equal),
                _ => None,
            },
        };
        match (op, order) {
            ("==", o) => o == Some(Ordering::Equal),
            ("!=", o) => o != Some(Ordering::Equal),
            ("<", Some(o)) => o == Ordering::Less,
            ("<=", Some(o)) => o != Ordering::Greater,
            (">", Some(o)) => o == Ordering::Greater,
            (">=", Some(o)) => o != Ordering::Less,
            _ => false,
        }
    }

    /// A number, or a string that parses as one.
    fn as_number(v: &Value) -> Option<f64> {
        match v {
            Value::Number(n) => n.as_f64(),
            Value::String(s) => s.trim().parse().ok(),
            _ => None,
        }
    }
}
