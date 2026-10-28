//! FastAPI's request-body handling and pydantic's validation errors.
//!
//! [`read_body`] follows fastapi/routing.py `get_request_handler` (FastAPI
//! 0.141, `strict_content_type=True`): an empty body is "no body"; only
//! `application/json` and `application/*+json` bodies are parsed (with
//! [`plumdeck_core::compat::pyjson_loads`], i.e. `json.loads`), anything else
//! is validated as raw bytes; a JSON syntax error is a 422 `json_invalid` and
//! an undecodable body a 400 "There was an error parsing the body".
//!
//! Models implement [`BodyModel`] with the helpers here, which produce
//! pydantic v2's lax-mode errors (`missing`, `string_type`,
//! `model_attributes_type`, ...). The 422 answer is FastAPI's
//! `{"detail": [{"type", "loc", "msg", "input", "ctx"?}, ...]}`, rendered by
//! Starlette's `json.dumps(..., allow_nan=False)`, so a NaN or Infinity in an
//! echoed input is a 500 there and here.

use axum::http::header::{self, HeaderMap};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use plumdeck_core::compat::pyjson::{dumps, DumpOptions, PyValue};
use plumdeck_core::compat::pyjson_loads::{loads_bytes, LoadsError};
use plumdeck_core::error::ApiError;

use crate::middleware::internal_server_error;
use crate::HttpError;

/// One element of `loc`.
#[derive(Clone, Debug, PartialEq)]
pub enum Loc {
    Key(String),
    Index(i128),
}

impl From<&str> for Loc {
    fn from(key: &str) -> Self {
        Self::Key(key.into())
    }
}

/// A pydantic error as FastAPI reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct ValidationError {
    pub kind: &'static str,
    pub loc: Vec<Loc>,
    pub msg: String,
    pub input: PyValue,
    pub ctx: Option<PyValue>,
}

impl ValidationError {
    pub fn new(kind: &'static str, loc: Vec<Loc>, msg: impl Into<String>, input: PyValue) -> Self {
        Self { kind, loc, msg: msg.into(), input, ctx: None }
    }

    fn to_py(&self) -> PyValue {
        let key = |name: &str| PyValue::Str(name.into());
        let loc = self
            .loc
            .iter()
            .map(|part| match part {
                Loc::Key(key) => PyValue::Str(key.clone()),
                Loc::Index(index) => PyValue::Int(*index),
            })
            .collect();
        let mut pairs = vec![
            (key("type"), PyValue::Str(self.kind.into())),
            (key("loc"), PyValue::List(loc)),
            (key("msg"), PyValue::Str(self.msg.clone())),
            (key("input"), self.input.clone()),
        ];
        if let Some(ctx) = &self.ctx {
            pairs.push((key("ctx"), ctx.clone()));
        }
        PyValue::Dict(pairs)
    }
}

/// FastAPI's `request_validation_exception_handler`.
pub fn validation_response(errors: &[ValidationError]) -> Response {
    let body = PyValue::Dict(vec![(
        PyValue::Str("detail".into()),
        PyValue::List(errors.iter().map(ValidationError::to_py).collect()),
    )]);
    match dumps(&body, &DumpOptions::starlette()) {
        Ok(text) => (StatusCode::UNPROCESSABLE_ENTITY, [(header::CONTENT_TYPE, "application/json")], text).into_response(),
        // json.dumps(allow_nan=False) raised: Starlette answers 500.
        Err(_) => internal_server_error(),
    }
}

/// `email.message.Message.get_content_type()` of a Content-Type value:
/// the part before `;`, stripped and lowercased, or `text/plain` unless it
/// has exactly one `/`.
pub fn content_type_of(value: &str) -> String {
    let main = value.split(';').next().unwrap_or("").trim().to_lowercase();
    if main.matches('/').count() == 1 { main } else { "text/plain".into() }
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(header::CONTENT_TYPE) else { return false };
    // Starlette decodes headers as Latin-1; an empty value counts as missing.
    let text: String = value.as_bytes().iter().map(|&b| char::from(b)).collect();
    if text.is_empty() {
        return false;
    }
    let content_type = content_type_of(&text);
    let (main, sub) = content_type.split_once('/').unwrap_or(("", ""));
    main == "application" && (sub == "json" || sub.ends_with("+json"))
}

/// The body FastAPI hands to validation: `None` when there is none (empty,
/// or JSON `null`), else the parsed JSON or the raw bytes as text.
pub fn read_body(headers: &HeaderMap, bytes: &[u8]) -> Result<Option<PyValue>, Response> {
    if bytes.is_empty() {
        return Ok(None);
    }
    if !is_json_content_type(headers) {
        // Raw bytes: jsonable_encoder echoes them decoded as UTF-8 (and
        // raises, i.e. 500, when they are not).
        return match std::str::from_utf8(bytes) {
            Ok(text) => Ok(Some(PyValue::Str(text.into()))),
            Err(_) => Err(internal_server_error()),
        };
    }
    match loads_bytes(bytes) {
        Ok(PyValue::None) => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(LoadsError::Decode(error)) => {
            let mut invalid = ValidationError::new(
                "json_invalid",
                vec![Loc::from("body"), Loc::Index(error.pos as i128)],
                "JSON decode error",
                PyValue::Dict(Vec::new()),
            );
            invalid.ctx = Some(PyValue::Dict(vec![(PyValue::Str("error".into()), PyValue::Str(error.msg.into()))]));
            Err(validation_response(&[invalid]))
        }
        Err(LoadsError::Other(_)) => {
            Err(HttpError(ApiError::bad_request("There was an error parsing the body")).into_response())
        }
    }
}

/// A request-body model (a pydantic `BaseModel` used as the single,
/// non-embedded body parameter).
pub trait BodyModel: Sized {
    /// Validate the fields of a JSON object; errors use `loc` `["body", field]`.
    fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>>;
}

/// The input object of a model, with the whole object kept for `missing`
/// errors (pydantic echoes the object there).
pub struct Fields<'a> {
    whole: &'a PyValue,
    pairs: &'a [(PyValue, PyValue)],
    /// `loc` of this object (`["body"]`, or `["body", "analyses", 0]`).
    prefix: Vec<Loc>,
    errors: std::cell::RefCell<Vec<ValidationError>>,
}

impl<'a> Fields<'a> {
    fn get(&self, name: &str) -> Option<&'a PyValue> {
        // json.loads keeps the last of duplicate keys.
        self.pairs.iter().rev().find(|(key, _)| matches!(key, PyValue::Str(k) if k == name)).map(|(_, value)| value)
    }

    fn push(&self, error: ValidationError) {
        self.errors.borrow_mut().push(error);
    }

    fn loc(&self, name: &str) -> Vec<Loc> {
        let mut loc = self.prefix.clone();
        loc.push(Loc::from(name));
        loc
    }

    fn missing(&self, name: &str) {
        self.push(ValidationError::new("missing", self.loc(name), "Field required", self.whole.clone()));
    }

    /// `Optional[str] = None`: absent or `null` is `None`.
    pub fn optional_str(&self, name: &str) -> Option<String> {
        match self.get(name) {
            None | Some(PyValue::None) => None,
            Some(PyValue::Str(text)) => Some(text.clone()),
            Some(other) => {
                self.push(ValidationError::new("string_type", self.loc(name), "Input should be a valid string", other.clone()));
                None
            }
        }
    }

    /// `str = default`: absent is the default, `null` is an error.
    pub fn str_or(&self, name: &str, default: &str) -> String {
        match self.get(name) {
            None => default.to_owned(),
            Some(_) => self.required_str(name).unwrap_or_default(),
        }
    }

    fn lax_int(&self, value: &PyValue, loc: Vec<Loc>) -> Option<i128> {
        let parsed = crate::lax::to_int(value);
        if parsed.is_none() {
            let (kind, msg) = match value {
                PyValue::Str(_) => ("int_parsing", "Input should be a valid integer, unable to parse string as an integer"),
                PyValue::Float(_) => ("int_from_float", "Input should be a valid integer, got a number with a fractional part"),
                _ => ("int_type", "Input should be a valid integer"),
            };
            self.push(ValidationError::new(kind, loc, msg, value.clone()));
        }
        parsed
    }

    /// `Optional[int] = None`.
    pub fn optional_int(&self, name: &str) -> Option<i128> {
        match self.get(name) {
            None | Some(PyValue::None) => None,
            Some(value) => self.lax_int(value, self.loc(name)),
        }
    }

    /// A required `int`.
    pub fn required_int(&self, name: &str) -> Option<i128> {
        match self.get(name) {
            None => {
                self.missing(name);
                None
            }
            Some(value) => self.lax_int(value, self.loc(name)),
        }
    }

    /// A required `List[int]` (JSON arrays only).
    pub fn int_list(&self, name: &str) -> Option<Vec<i128>> {
        match self.get(name) {
            None => {
                self.missing(name);
                None
            }
            Some(PyValue::List(items)) => {
                let values: Vec<Option<i128>> = items
                    .iter()
                    .enumerate()
                    .map(|(index, item)| {
                        let mut loc = self.loc(name);
                        loc.push(Loc::Index(index as i128));
                        self.lax_int(item, loc)
                    })
                    .collect();
                values.into_iter().collect()
            }
            Some(other) => {
                self.push(ValidationError::new("list_type", self.loc(name), "Input should be a valid list", other.clone()));
                None
            }
        }
    }

    /// `bool = default`.
    pub fn bool_or(&self, name: &str, default: bool) -> bool {
        match self.get(name) {
            None => default,
            Some(value) => crate::lax::to_bool(value).unwrap_or_else(|| {
                self.push(ValidationError::new("bool_parsing", self.loc(name), "Input should be a valid boolean", value.clone()));
                default
            }),
        }
    }

    /// A `str` Enum field with a default: the value must be one of `choices`.
    pub fn choice_or(&self, name: &str, default: &str, choices: &[&str]) -> String {
        match self.get(name) {
            None => default.to_owned(),
            Some(PyValue::Str(text)) if choices.contains(&text.as_str()) => text.clone(),
            Some(other) => {
                self.push(ValidationError::new("enum", self.loc(name), "Input should be one of the allowed values", other.clone()));
                default.to_owned()
            }
        }
    }

    /// A required `List[Model]`.
    pub fn model_list<T>(&self, name: &str, item: impl Fn(&Fields<'_>) -> Option<T>) -> Option<Vec<T>> {
        let items = match self.get(name) {
            None => {
                self.missing(name);
                return None;
            }
            Some(PyValue::List(items)) => items,
            Some(other) => {
                self.push(ValidationError::new("list_type", self.loc(name), "Input should be a valid list", other.clone()));
                return None;
            }
        };
        let mut out = Vec::new();
        let mut failed = false;
        for (index, value) in items.iter().enumerate() {
            let mut prefix = self.loc(name);
            prefix.push(Loc::Index(index as i128));
            match value {
                PyValue::Dict(pairs) => {
                    let fields = Fields { whole: value, pairs, prefix, errors: std::cell::RefCell::new(Vec::new()) };
                    let parsed = item(&fields);
                    let errors = fields.errors.take();
                    if errors.is_empty() {
                        out.extend(parsed);
                    } else {
                        failed = true;
                        self.errors.borrow_mut().extend(errors);
                    }
                }
                other => {
                    failed = true;
                    self.push(ValidationError::new(
                        "model_attributes_type",
                        prefix,
                        "Input should be a valid dictionary or object to extract fields from",
                        other.clone(),
                    ));
                }
            }
        }
        (!failed).then_some(out)
    }

    /// A required `str` field: pydantic lax mode accepts only strings from
    /// JSON input.
    pub fn required_str(&self, name: &str) -> Option<String> {
        let loc = self.loc(name);
        match self.get(name) {
            None => {
                self.push(ValidationError::new("missing", loc, "Field required", self.whole.clone()));
                None
            }
            Some(PyValue::Str(text)) => Some(text.clone()),
            Some(other) => {
                self.push(ValidationError::new("string_type", loc, "Input should be a valid string", other.clone()));
                None
            }
        }
    }

    /// The raw value of `name` (the last of duplicate keys).
    pub fn value(&self, name: &str) -> Option<&'a PyValue> {
        self.get(name)
    }

    /// The names present, in order (duplicates once, last wins).
    pub fn names(&self) -> Vec<&'a str> {
        let mut names: Vec<&str> = Vec::new();
        for (key, _) in self.pairs {
            if let PyValue::Str(key) = key {
                names.retain(|name| name != key);
                names.push(key);
            }
        }
        names
    }

    /// Record a validation error on `name`.
    pub fn reject(&self, name: &str, kind: &'static str, msg: &str) {
        let input = self.get(name).cloned().unwrap_or(PyValue::None);
        self.push(ValidationError::new(kind, self.loc(name), msg, input));
    }

    /// `int = default` (lax).
    pub fn int_or(&self, name: &str, default: i128) -> i128 {
        match self.get(name) {
            None => default,
            Some(value) => self.lax_int(value, self.loc(name)).unwrap_or(default),
        }
    }

    /// `Optional[List[int]] = None` (lax items).
    pub fn optional_int_list(&self, name: &str) -> Option<Vec<i128>> {
        match self.get(name) {
            None | Some(PyValue::None) => None,
            Some(_) => self.int_list(name),
        }
    }

    /// `Optional[List[str]] = None`.
    pub fn optional_str_list(&self, name: &str) -> Option<Vec<String>> {
        match self.get(name) {
            None | Some(PyValue::None) => None,
            Some(PyValue::List(items)) => {
                let mut out = Vec::new();
                let mut failed = false;
                for (index, item) in items.iter().enumerate() {
                    match item {
                        PyValue::Str(text) => out.push(text.clone()),
                        other => {
                            failed = true;
                            let mut loc = self.loc(name);
                            loc.push(Loc::Index(index as i128));
                            self.push(ValidationError::new("string_type", loc, "Input should be a valid string", other.clone()));
                        }
                    }
                }
                (!failed).then_some(out)
            }
            Some(other) => {
                self.push(ValidationError::new("list_type", self.loc(name), "Input should be a valid list", other.clone()));
                None
            }
        }
    }

    /// The collected errors, or `value` when there are none.
    pub fn finish<T>(&self, value: impl FnOnce() -> Option<T>) -> Result<T, Vec<ValidationError>> {
        let errors = self.errors.take();
        if !errors.is_empty() {
            return Err(errors);
        }
        value().ok_or_else(Vec::new)
    }
}

/// Validate `body` (from [`read_body`]) as model `T`.
pub fn validate<T: BodyModel>(body: Option<&PyValue>) -> Result<T, Vec<ValidationError>> {
    let body_loc = || vec![Loc::from("body")];
    match body {
        None => Err(vec![ValidationError::new("missing", body_loc(), "Field required", PyValue::None)]),
        Some(whole @ PyValue::Dict(pairs)) => {
            T::from_fields(&Fields { whole, pairs, prefix: body_loc(), errors: std::cell::RefCell::new(Vec::new()) })
        }
        Some(other) => Err(vec![ValidationError::new(
            "model_attributes_type",
            body_loc(),
            "Input should be a valid dictionary or object to extract fields from",
            other.clone(),
        )]),
    }
}

/// Validate embedded body parameters (`Body(embed=True)`, or several body
/// parameters): no body at all is an empty object, so only required
/// fields report `missing`.
pub fn validate_embedded<T: BodyModel>(body: Option<&PyValue>) -> Result<T, Vec<ValidationError>> {
    let empty = PyValue::Dict(Vec::new());
    validate(Some(body.unwrap_or(&empty)))
}

/// [`read_body`] then [`validate_embedded`], answering FastAPI's errors.
pub fn parse_embedded<T: BodyModel>(headers: &HeaderMap, bytes: &[u8]) -> Result<T, Response> {
    let body = read_body(headers, bytes)?;
    validate_embedded(body.as_ref()).map_err(|errors| validation_response(&errors))
}

/// [`read_body`] then [`validate`], answering FastAPI's errors.
pub fn parse_body<T: BodyModel>(headers: &HeaderMap, bytes: &[u8]) -> Result<T, Response> {
    let body = read_body(headers, bytes)?;
    validate(body.as_ref()).map_err(|errors| validation_response(&errors))
}

/// [`parse_body`] for routes that also validate parameters: a body that
/// cannot be decoded answers at once (FastAPI parses it before validating
/// anything), validation errors are returned to join the parameters' ones.
pub fn parse_body_for<T: BodyModel>(headers: &HeaderMap, bytes: &[u8]) -> Result<Result<T, Vec<ValidationError>>, Response> {
    let body = read_body(headers, bytes)?;
    Ok(validate(body.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::http::HeaderValue;

    #[derive(Debug, PartialEq)]
    struct Pair {
        key: String,
        value: String,
    }

    impl BodyModel for Pair {
        fn from_fields(fields: &Fields<'_>) -> Result<Self, Vec<ValidationError>> {
            let key = fields.required_str("key");
            let value = fields.required_str("value");
            fields.finish(|| Some(Self { key: key?, value: value? }))
        }
    }

    fn headers(content_type: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(value) = content_type {
            headers.insert(header::CONTENT_TYPE, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    async fn answer(content_type: Option<&str>, body: &[u8]) -> (u16, String) {
        let response = match parse_body::<Pair>(&headers(content_type), body) {
            Ok(pair) => return (200, format!("{pair:?}")),
            Err(response) => response,
        };
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    // Expected bodies: FastAPI 0.141.1 / pydantic 2.13.5 on POST /api/settings.
    #[tokio::test]
    async fn errors_match_fastapi() {
        let json = Some("application/json");
        assert_eq!(
            answer(json, b"").await,
            (422, r#"{"detail":[{"type":"missing","loc":["body"],"msg":"Field required","input":null}]}"#.into())
        );
        assert_eq!(answer(json, b"null").await.1, answer(json, b"").await.1);
        assert_eq!(
            answer(json, b"{}").await.1,
            r#"{"detail":[{"type":"missing","loc":["body","key"],"msg":"Field required","input":{}},{"type":"missing","loc":["body","value"],"msg":"Field required","input":{}}]}"#
        );
        assert_eq!(
            answer(json, b"[]").await.1,
            r#"{"detail":[{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":[]}]}"#
        );
        assert_eq!(
            answer(json, br#"{"key":1,"value":true}"#).await.1,
            r#"{"detail":[{"type":"string_type","loc":["body","key"],"msg":"Input should be a valid string","input":1},{"type":"string_type","loc":["body","value"],"msg":"Input should be a valid string","input":true}]}"#
        );
        assert_eq!(
            answer(json, b"{bad").await.1,
            r#"{"detail":[{"type":"json_invalid","loc":["body",1],"msg":"JSON decode error","input":{},"ctx":{"error":"Expecting property name enclosed in double quotes"}}]}"#
        );
        assert_eq!(
            answer(Some("text/plain"), br#"{"key":"k","value":"v"}"#).await.1,
            r#"{"detail":[{"type":"model_attributes_type","loc":["body"],"msg":"Input should be a valid dictionary or object to extract fields from","input":"{\"key\":\"k\",\"value\":\"v\"}"}]}"#
        );
        assert_eq!(answer(None, br#"{"key":"k","value":"v"}"#).await.0, 422);
        assert_eq!(answer(Some(""), br#"{"key":"k","value":"v"}"#).await.0, 422);
        assert_eq!(
            answer(json, b"{\"key\":\"\xff\",\"value\":\"v\"}").await,
            (400, r#"{"detail":"There was an error parsing the body"}"#.into())
        );
        // NaN in an echoed input: json.dumps(allow_nan=False) fails.
        assert_eq!(answer(json, br#"{"key":"a","value":NaN}"#).await.0, 500);
    }

    #[tokio::test]
    async fn accepted_bodies() {
        let ok = |key: &str, value: &str| (200, format!("{:?}", Pair { key: key.into(), value: value.into() }));
        assert_eq!(answer(Some("application/json"), br#"{"key":"a","value":"b","extra":NaN}"#).await, ok("a", "b"));
        assert_eq!(
            answer(Some("Application/Vnd.X+JSON; charset=utf-8"), "{\"key\":\"日本\",\"value\":\"\"}".as_bytes()).await,
            ok("日本", "")
        );
        assert_eq!(answer(Some("application/json"), b"\xef\xbb\xbf{\"key\":\"a\",\"value\":\"b\"}").await, ok("a", "b"));
        assert_eq!(content_type_of(" Application/JSON ; x=1"), "application/json");
        assert_eq!(content_type_of("json"), "text/plain");
        assert_eq!(content_type_of("a/b/c"), "text/plain");
    }
}
