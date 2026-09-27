//! 하늘 (空): a web framework for Haru in the manner of Flask.
//!
//! ```text
//! [하늘]에서 <앱>을 가져오자
//! '앱'을 <앱>()으로 정하자
//! <인사>를 만들자 ('요청'):
//!     틀"안녕, {'요청'의 <인자>("이름")}!"을 돌려주자
//! '앱'의 <GET>("/안녕/<이름>", <인사>)를 실행하자
//! '앱'의 <실행>(5000)을 실행하자
//! ```
//!
//! Everything is native (no source entry points): the app, the request and
//! the response are resources whose methods have a name in each language.
//! Handlers run on the program's thread while `<실행>` waits, as in the
//! http_server package: connections are read on other threads and handed
//! over as plain data (`http`). What a handler returns becomes the response:
//! a text is HTML, a dictionary or list is JSON, `비어있음` is 204, or a
//! response made with `<응답>` and its kin.

mod data;
mod http;
mod router;
mod template;
mod util;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use haru_sdk::abi::tag;
use haru_sdk::prelude::*;
use haru_sdk::IntoRet;

use data::{from_value, parse_json, to_json, to_value, Lang, V};
use http::{Answer, Incoming, Job};
use router::{Match, Rule, RuleError};
use util::{html_escape, log_date, mime_for, now_secs, parse_cookies, parse_query, percent_decode, set_cookie, sign, status_text, unsign, CookieOptions};

const SESSION_COOKIE: &str = "session";

// ---------------------------------------------------------------------------
// The module

fn build(m: &mut Module) {
    m.name("hari", "하늘").name("kanade", "空");

    // One constructor per language, so the app knows which words to speak.
    m.func("app", || new_app(Lang::Hari)).name("hari", "앱");
    m.func("app_kanade", || new_app(Lang::Kanade)).name("kanade", "アプリ");

    m.func("response", |body: Option<Value>, status: Option<f64>, headers: Option<Dict>| make_response(body, status, headers))
        .name("hari", "응답")
        .name("kanade", "応答");
    m.func("json", |value: Value, status: Option<f64>| {
        let r = make_response(None, status, None)?;
        let text = json_text(&value)?;
        r.set_header("Content-Type", "application/json");
        *r.body.borrow_mut() = text.into_bytes();
        Ok(r)
    })
    .name("hari", "JSON응답")
    .name("kanade", "JSON応答");
    m.func("redirect", |url: Str, status: Option<f64>| {
        let status = status_of(status.unwrap_or(302.0))?;
        let body = format!("<!doctype html>\n<a href=\"{0}\">{0}</a>\n", html_escape(&url));
        let r = Response::new(status, Some("text/html; charset=utf-8"), body.into_bytes());
        r.set_header("Location", &url);
        Res::new(r)
    })
    .name("hari", "리다이렉트")
    .name("kanade", "リダイレクト");
    m.func("abort", |status: f64, message: Option<Str>| -> Result<()> {
        Err(abort(status_of(status)?, message.map(|m| m.to_string())))
    })
    .name("hari", "중단")
    .name("kanade", "中断");
    m.func("render", |name: Str, vars: Option<Dict>| render_file(&name, vars, Lang::Hari)).name("hari", "템플릿");
    m.func("render_kanade", |name: Str, vars: Option<Dict>| render_file(&name, vars, Lang::Kanade)).name("kanade", "テンプレート");
    m.func("render_string", |text: Str, vars: Option<Dict>| render_text(&text, vars, Lang::Hari)).name("hari", "템플릿문자열");
    m.func("render_string_kanade", |text: Str, vars: Option<Dict>| render_text(&text, vars, Lang::Kanade))
        .name("kanade", "テンプレート文字列");
    m.func("send_file", |path: Str, content_type: Option<Str>| send_file(Path::new(&*path), content_type.as_deref()))
        .name("hari", "파일보내기")
        .name("kanade", "ファイル送信");
    m.func("escape", |text: Str| html_escape(&text)).name("hari", "이스케이프").name("kanade", "エスケープ");

    describe_app(m);
    describe_request(m);
    describe_response(m);

    for (code, ko, ja) in [
        ("Abort", "요청을 중단했어요 ({0}).", "リクエストを中断しました（{0}）。"),
        ("BadRule", "경로 규칙 '{0}'이 올바르지 않아요: {1}", "ルート「{0}」が正しくありません: {1}"),
        ("BadMethod", "HTTP 메서드는 글이나 글의 목록으로 줘요.", "HTTPメソッドは文字列か文字列のリストで渡します。"),
        ("BadStatus", "HTTP 상태 코드는 100부터 599까지의 정수예요: {0}", "HTTPステータスコードは100から599までの整数です: {0}"),
        ("BadBody", "본문으로 쓸 수 없는 값이에요: {0} (글, 사전, 목록, 비어있음 중 하나로 줘요)", "本文にできない値です: {0}（文字列・辞書・リスト・空っぽのどれかを渡します）"),
        ("BadPort", "포트는 0부터 65535까지의 정수예요: {0}", "ポートは0から65535までの整数です: {0}"),
        ("BadTarget", "요청 경로는 '/'로 시작해요: {0}", "リクエストのパスは「/」で始めます: {0}"),
        ("AlreadyRunning", "앱이 이미 실행 중이에요.", "アプリはすでに実行中です。"),
        ("Listen", "{0}에서 요청을 기다릴 수 없어요: {1}", "{0}で待ち受けできません: {1}"),
        (
            "NoSecretKey",
            "세션을 쓰려면 먼저 '앱'의 <비밀키>(\"...\")로 비밀키를 정해 주세요.",
            "セッションを使うには、先に『アプリ』の〈秘密鍵〉(「...」)で秘密鍵を決めてください。",
        ),
        ("Template", "템플릿 오류: {0}", "テンプレートエラー: {0}"),
        ("NotJSON", "JSON으로 바꿀 수 없는 값이에요: {0}", "JSONにできない値です: {0}"),
    ] {
        m.message(code, "hari", ko).message(code, "kanade", ja);
    }
}

haru_sdk::export!("haneul", build);

// ---------------------------------------------------------------------------
// The app

struct Route {
    methods: Vec<String>,
    rule: Rule,
    handler: Value,
}

impl Route {
    fn accepts(&self, method: &str) -> bool {
        self.methods.iter().any(|m| m == method || (method == "HEAD" && m == "GET"))
    }
}

struct App {
    lang: Lang,
    routes: RefCell<Vec<Rc<Route>>>,
    before: RefCell<Vec<Value>>,
    after: RefCell<Vec<Value>>,
    errors: RefCell<Vec<(u16, Value)>>,
    /// URL prefix (no `/` at the end) and folder.
    statics: RefCell<Vec<(String, PathBuf)>>,
    templates: RefCell<PathBuf>,
    secret: RefCell<Option<Rc<[u8]>>>,
    debug: Cell<bool>,
    log: Cell<bool>,
    running: RefCell<Option<Arc<AtomicBool>>>,
    /// The cookies `<시험요청>` keeps between requests, like a browser.
    test_cookies: RefCell<Vec<(String, String)>>,
}

fn new_app(lang: Lang) -> Result<Res<App>> {
    Res::new(App {
        lang,
        routes: RefCell::new(Vec::new()),
        before: RefCell::new(Vec::new()),
        after: RefCell::new(Vec::new()),
        errors: RefCell::new(Vec::new()),
        statics: RefCell::new(vec![("/static".into(), PathBuf::from("static"))]),
        templates: RefCell::new(PathBuf::from("templates")),
        secret: RefCell::new(None),
        debug: Cell::new(false),
        log: Cell::new(true),
        running: RefCell::new(None),
        test_cookies: RefCell::new(Vec::new()),
    })
}

fn func_value(f: Func) -> Result<Value> {
    f.into_ret()
}

impl App {
    fn add_route(&self, methods: Vec<String>, path: &str, handler: Func) -> Result<()> {
        let rule = Rule::parse(path).map_err(|e| {
            let why = match e {
                RuleError::NoLeadingSlash => self.lang.tr("'/'로 시작해야 해요.", "「/」で始める必要があります。"),
                RuleError::BadVariable(p) => self.lang.tr(&format!("변수 '{p}'를 읽을 수 없어요."), &format!("変数「{p}」を読めません。")),
                RuleError::UnknownConverter(c) => self.lang.tr(
                    &format!("'{c}'는 알 수 없는 변환이에요 (string, int, float, path 중 하나)."),
                    &format!("「{c}」は知らない変換です（string・int・float・pathのどれか）。"),
                ),
            };
            Error::new("BadRule").arg(path).arg(why)
        })?;
        let handler = func_value(handler)?;
        self.routes.borrow_mut().push(Rc::new(Route { methods, rule, handler }));
        Ok(())
    }
}

fn methods_of(v: &Value) -> Result<Vec<String>> {
    let bad = || Error::new("BadMethod");
    let names: Vec<String> = match v.tag() {
        tag::STR => vec![v.as_str().unwrap().to_string()],
        tag::LIST => v.as_list().unwrap().iter().map(|m| m.as_str().map(|s| s.to_string()).ok_or_else(bad)).collect::<Result<_>>()?,
        _ => return Err(bad()),
    };
    if names.is_empty() || names.iter().any(|n| n.is_empty() || !n.bytes().all(|c| c.is_ascii_alphabetic())) {
        return Err(bad());
    }
    Ok(names.into_iter().map(|n| n.to_ascii_uppercase()).collect())
}

fn describe_app(m: &mut Module) {
    let app = m.resource::<App>("app");
    app.name("hari", "앱").name("kanade", "アプリ");
    for (id, method) in [("get", "GET"), ("post", "POST"), ("put", "PUT"), ("patch", "PATCH"), ("delete", "DELETE")] {
        app.method(id, move |a: Res<App>, path: Str, f: Func| a.add_route(vec![method.to_string()], &path, f))
            .name("hari", method)
            .name("kanade", method);
    }
    app.method("route", |a: Res<App>, methods: Value, path: Str, f: Func| a.add_route(methods_of(&methods)?, &path, f))
        .name("hari", "라우트")
        .name("kanade", "ルート");
    app.method("before", |a: Res<App>, f: Func| -> Result<()> {
        a.before.borrow_mut().push(func_value(f)?);
        Ok(())
    })
    .name("hari", "요청전")
    .name("kanade", "リクエスト前");
    app.method("after", |a: Res<App>, f: Func| -> Result<()> {
        a.after.borrow_mut().push(func_value(f)?);
        Ok(())
    })
    .name("hari", "요청후")
    .name("kanade", "リクエスト後");
    app.method("error_handler", |a: Res<App>, status: f64, f: Func| -> Result<()> {
        let status = status_of(status)?;
        let f = func_value(f)?;
        let mut errors = a.errors.borrow_mut();
        errors.retain(|(s, _)| *s != status);
        errors.push((status, f));
        Ok(())
    })
    .name("hari", "오류처리")
    .name("kanade", "エラー処理");
    app.method("static_folder", |a: Res<App>, prefix: Str, folder: Str| {
        let prefix = format!("/{}", prefix.trim_matches('/'));
        let mut statics = a.statics.borrow_mut();
        statics.retain(|(p, _)| *p != prefix);
        statics.push((prefix, PathBuf::from(&*folder)));
    })
    .name("hari", "정적폴더")
    .name("kanade", "静的フォルダ");
    app.method("template_folder", |a: Res<App>, folder: Str| {
        *a.templates.borrow_mut() = PathBuf::from(&*folder);
    })
    .name("hari", "템플릿폴더")
    .name("kanade", "テンプレートフォルダ");
    app.method("secret_key", |a: Res<App>, key: Str| {
        *a.secret.borrow_mut() = Some(Rc::from(key.as_bytes()));
    })
    .name("hari", "비밀키")
    .name("kanade", "秘密鍵");
    app.method("debug", |a: Res<App>, on: bool| a.debug.set(on)).name("hari", "디버그").name("kanade", "デバッグ");
    app.method("log", |a: Res<App>, on: bool| a.log.set(on)).name("hari", "기록").name("kanade", "ログ");
    app.method("run", run).name("hari", "실행").name("kanade", "実行");
    app.method("stop", |a: Res<App>| {
        if let Some(stop) = a.running.borrow().as_ref() {
            stop.store(true, Ordering::SeqCst);
        }
    })
    .name("hari", "멈추기")
    .name("kanade", "停止");
    app.method("test", test_request).name("hari", "시험요청").name("kanade", "テストリクエスト");
}

// ---------------------------------------------------------------------------
// The request

struct Request {
    lang: Lang,
    method: String,
    /// The path as it came (still `%`-encoded) and decoded.
    raw_path: String,
    path: String,
    query_string: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    remote: String,
    params: RefCell<Vec<(String, V)>>,
    cookies: Vec<(String, String)>,
    secret: Option<Rc<[u8]>>,
    /// The session dictionary once asked for, and its JSON when it was read.
    session: RefCell<Option<(Value, String)>>,
    store: RefCell<Vec<(String, Value)>>,
}

impl Request {
    fn new(app: &App, inc: Incoming) -> Request {
        let (raw_path, query_string) = match inc.target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (inc.target.clone(), String::new()),
        };
        let cookies = inc
            .headers
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("Cookie"))
            .flat_map(|(_, v)| parse_cookies(v))
            .collect();
        Request {
            lang: app.lang,
            method: inc.method.to_ascii_uppercase(),
            path: percent_decode(&raw_path, false),
            raw_path,
            query: parse_query(&query_string),
            query_string,
            headers: inc.headers,
            body: inc.body,
            remote: inc.remote,
            params: RefCell::new(Vec::new()),
            cookies,
            secret: app.secret.borrow().clone(),
            session: RefCell::new(None),
            store: RefCell::new(Vec::new()),
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn form(&self) -> Vec<(String, String)> {
        let urlencoded = self
            .header("Content-Type")
            .is_some_and(|t| t.to_ascii_lowercase().starts_with("application/x-www-form-urlencoded"));
        if urlencoded {
            parse_query(&String::from_utf8_lossy(&self.body))
        } else {
            Vec::new()
        }
    }

    fn session(&self) -> Result<Value> {
        if let Some((d, _)) = &*self.session.borrow() {
            return Ok(d.clone());
        }
        let key = self.secret.clone().ok_or_else(|| Error::new("NoSecretKey"))?;
        let stored = self
            .cookies
            .iter()
            .find(|(k, _)| k == SESSION_COOKIE)
            .and_then(|(_, c)| unsign(&key, c))
            .and_then(|t| parse_json(&t))
            .filter(|v| matches!(v, V::Dict(_)));
        let (value, text) = match stored {
            Some(v) => (to_value(&v)?, to_json(&v, false).unwrap_or_default()),
            None => (Dict::new().into_ret()?, "{}".to_string()),
        };
        *self.session.borrow_mut() = Some((value.clone(), text));
        Ok(value)
    }
}

/// The first value of a name among pairs.
fn first(pairs: &[(String, String)], name: &str) -> Option<String> {
    pairs.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
}

/// A dictionary of pairs (the first value of each name).
fn pairs_dict(pairs: &[(String, String)]) -> Result<Dict> {
    let d = Dict::new();
    for (k, v) in pairs.iter().rev() {
        d.set(k.as_str(), v.as_str())?;
    }
    Ok(d)
}

fn all_of(pairs: &[(String, String)], name: &str) -> Vec<String> {
    pairs.iter().filter(|(k, _)| k == name).map(|(_, v)| v.clone()).collect()
}

fn describe_request(m: &mut Module) {
    let req = m.resource::<Request>("request");
    req.name("hari", "요청").name("kanade", "リクエスト");
    req.method("method", |r: Res<Request>| r.method.clone()).name("hari", "메서드").name("kanade", "メソッド");
    req.method("path", |r: Res<Request>| r.path.clone()).name("hari", "경로").name("kanade", "パス");
    req.method("query_string", |r: Res<Request>| r.query_string.clone()).name("hari", "쿼리문자열").name("kanade", "クエリ文字列");
    req.method("param", |r: Res<Request>, name: Str| -> Result<Value> {
        match r.params.borrow().iter().find(|(k, _)| *k == *name) {
            Some((_, v)) => to_value(v),
            None => Ok(Value::NULL),
        }
    })
    .name("hari", "인자")
    .name("kanade", "引数");
    req.method("params", |r: Res<Request>| -> Result<Dict> {
        let d = Dict::new();
        for (k, v) in r.params.borrow().iter() {
            d.set(k.as_str(), to_value(v)?)?;
        }
        Ok(d)
    })
    .name("hari", "인자들")
    .name("kanade", "引数一覧");
    req.method("query", |r: Res<Request>, name: Str, default: Option<Value>| -> Result<Value> {
        match first(&r.query, &name) {
            Some(v) => Ok(Value::str(&v)),
            None => Ok(default.unwrap_or(Value::NULL)),
        }
    })
    .name("hari", "쿼리")
    .name("kanade", "クエリ");
    req.method("queries", |r: Res<Request>| pairs_dict(&r.query)).name("hari", "쿼리들").name("kanade", "クエリ一覧");
    req.method("query_list", |r: Res<Request>, name: Str| all_of(&r.query, &name)).name("hari", "쿼리목록").name("kanade", "クエリリスト");
    req.method("form", |r: Res<Request>, name: Str, default: Option<Value>| -> Result<Value> {
        match first(&r.form(), &name) {
            Some(v) => Ok(Value::str(&v)),
            None => Ok(default.unwrap_or(Value::NULL)),
        }
    })
    .name("hari", "폼")
    .name("kanade", "フォーム");
    req.method("forms", |r: Res<Request>| pairs_dict(&r.form())).name("hari", "폼들").name("kanade", "フォーム一覧");
    req.method("form_list", |r: Res<Request>, name: Str| all_of(&r.form(), &name)).name("hari", "폼목록").name("kanade", "フォームリスト");
    req.method("header", |r: Res<Request>, name: Str| r.header(&name).map(str::to_string)).name("hari", "헤더").name("kanade", "ヘッダー");
    req.method("headers", |r: Res<Request>| -> Result<Dict> {
        let lower: Vec<(String, String)> = r.headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.clone())).collect();
        pairs_dict(&lower)
    })
    .name("hari", "헤더들")
    .name("kanade", "ヘッダー一覧");
    req.method("cookie", |r: Res<Request>, name: Str| first(&r.cookies, &name)).name("hari", "쿠키").name("kanade", "クッキー");
    req.method("cookies", |r: Res<Request>| pairs_dict(&r.cookies)).name("hari", "쿠키들").name("kanade", "クッキー一覧");
    req.method("body", |r: Res<Request>| String::from_utf8_lossy(&r.body).into_owned()).name("hari", "본문").name("kanade", "本文");
    req.method("json", |r: Res<Request>| -> Result<Value> {
        let text = String::from_utf8_lossy(&r.body);
        if text.trim().is_empty() {
            return Ok(Value::NULL);
        }
        match parse_json(&text) {
            Some(v) => to_value(&v),
            None => Err(abort(400, Some(r.lang.tr("본문이 올바른 JSON이 아니에요.", "本文が正しいJSONではありません。")))),
        }
    })
    .name("hari", "JSON")
    .name("kanade", "JSON");
    req.method("remote", |r: Res<Request>| match r.remote.parse::<std::net::SocketAddr>() {
        Ok(a) => a.ip().to_string(),
        Err(_) => r.remote.clone(),
    })
    .name("hari", "주소")
    .name("kanade", "アドレス");
    req.method("session", |r: Res<Request>| r.session()).name("hari", "세션").name("kanade", "セッション");
    // A dictionary's missing key is an error in the language: these read and
    // change the session without that.
    req.method("session_get", |r: Res<Request>, key: Value, default: Option<Value>| -> Result<Value> {
        let d = r.session()?.as_dict().unwrap_or_default();
        Ok(d.get(&key).or(default).unwrap_or(Value::NULL))
    })
    .name("hari", "세션값")
    .name("kanade", "セッション値");
    req.method("session_set", |r: Res<Request>, key: Value, value: Value| -> Result<()> {
        r.session()?.as_dict().unwrap_or_default().set(key, value)
    })
    .name("hari", "세션설정")
    .name("kanade", "セッション設定");
    req.method("session_delete", |r: Res<Request>, key: Value| -> Result<bool> {
        Ok(r.session()?.as_dict().unwrap_or_default().remove(&key))
    })
    .name("hari", "세션삭제")
    .name("kanade", "セッション削除");
    req.method("session_clear", |r: Res<Request>| -> Result<()> {
        let d = r.session()?.as_dict().unwrap_or_default();
        for k in d.keys().iter() {
            d.remove(&k);
        }
        Ok(())
    })
    .name("hari", "세션비우기")
    .name("kanade", "セッション消去");
    req.method("set", |r: Res<Request>, key: Str, value: Value| {
        let mut store = r.store.borrow_mut();
        store.retain(|(k, _)| *k != *key);
        store.push((key.to_string(), value));
    })
    .name("hari", "넣기")
    .name("kanade", "入れる");
    req.method("get", |r: Res<Request>, key: Str, default: Option<Value>| {
        r.store.borrow().iter().find(|(k, _)| *k == *key).map(|(_, v)| v.clone()).or(default).unwrap_or(Value::NULL)
    })
    .name("hari", "꺼내기")
    .name("kanade", "取り出す");
}

// ---------------------------------------------------------------------------
// The response

struct Response {
    status: Cell<u16>,
    headers: RefCell<Vec<(String, String)>>,
    body: RefCell<Vec<u8>>,
}

impl Response {
    fn new(status: u16, content_type: Option<&str>, body: Vec<u8>) -> Response {
        let headers = content_type.map(|t| vec![("Content-Type".to_string(), t.to_string())]).unwrap_or_default();
        Response { status: Cell::new(status), headers: RefCell::new(headers), body: RefCell::new(body) }
    }

    fn set_header(&self, name: &str, value: &str) {
        let mut h = self.headers.borrow_mut();
        h.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        h.push((name.to_string(), value.to_string()));
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers.borrow().iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone())
    }

    fn answer(&self) -> Answer {
        Answer { status: self.status.get(), headers: self.headers.borrow().clone(), body: self.body.borrow().clone() }
    }
}

fn status_of(n: f64) -> Result<u16> {
    if n.fract() == 0.0 && (100.0..=599.0).contains(&n) {
        Ok(n as u16)
    } else {
        Err(Error::new("BadStatus").arg(n))
    }
}

/// A value as JSON text (the error names what cannot be written).
fn json_text(v: &Value) -> Result<String> {
    to_json(&from_value(v, Lang::Hari), false).map_err(|what| Error::new("NotJSON").arg(what))
}

/// A value as a body: a text is HTML, a dictionary or list is JSON.
fn body_of(v: &Value) -> Result<(Option<&'static str>, Vec<u8>)> {
    Ok(match v.tag() {
        tag::NULL => (None, Vec::new()),
        tag::STR => (Some("text/html; charset=utf-8"), v.as_str().unwrap().as_bytes().to_vec()),
        tag::LIST | tag::DICT => (Some("application/json"), json_text(v)?.into_bytes()),
        _ => return Err(Error::new("BadBody").arg(from_value(v, Lang::Hari).text(Lang::Hari))),
    })
}

fn make_response(body: Option<Value>, status: Option<f64>, headers: Option<Dict>) -> Result<Res<Response>> {
    let status = status_of(status.unwrap_or(200.0))?;
    let (ctype, bytes) = body_of(&body.unwrap_or(Value::NULL))?;
    let r = Response::new(status, ctype, bytes);
    if let Some(h) = headers {
        for k in h.keys().iter() {
            let v = h.get(&k).unwrap_or(Value::NULL);
            let (k, v) = (from_value(&k, Lang::Hari).text(Lang::Hari), from_value(&v, Lang::Hari).text(Lang::Hari));
            r.set_header(&k, &v);
        }
    }
    Res::new(r)
}

fn cookie_options(o: Option<Dict>) -> CookieOptions {
    let mut opts = CookieOptions::default();
    let Some(o) = o else { return opts };
    let o = from_value(&o.into_ret().unwrap_or(Value::NULL), Lang::Hari);
    let text = |k: &str| o.get_str(k).and_then(|v| v.as_text().map(str::to_string));
    if let Some(V::Num(n)) = o.get_str("max_age") {
        opts.max_age = Some(*n as i64);
    }
    if let Some(e) = text("expires") {
        opts.expires = Some(e);
    }
    if let Some(p) = text("path") {
        opts.path = p;
    }
    opts.domain = text("domain");
    opts.secure = o.get_str("secure").is_some_and(V::truthy);
    opts.http_only = o.get_str("httponly").or_else(|| o.get_str("http_only")).is_some_and(V::truthy);
    opts.same_site = text("samesite").or_else(|| text("same_site"));
    opts
}

fn describe_response(m: &mut Module) {
    let resp = m.resource::<Response>("response");
    resp.name("hari", "응답").name("kanade", "応答");
    resp.method("status", |r: Res<Response>| r.status.get() as f64).name("hari", "상태").name("kanade", "状態");
    resp.method("body", |r: Res<Response>| String::from_utf8_lossy(&r.body.borrow()).into_owned()).name("hari", "본문").name("kanade", "本文");
    resp.method("json", |r: Res<Response>| -> Result<Value> {
        let text = String::from_utf8_lossy(&r.body.borrow()).into_owned();
        parse_json(&text).map_or(Ok(Value::NULL), |v| to_value(&v))
    })
    .name("hari", "JSON")
    .name("kanade", "JSON");
    resp.method("header", |r: Res<Response>, name: Str| r.header(&name)).name("hari", "헤더").name("kanade", "ヘッダー");
    resp.method("headers", |r: Res<Response>| pairs_dict(&r.headers.borrow())).name("hari", "헤더들").name("kanade", "ヘッダー一覧");
    resp.method("set_status", |r: Res<Response>, status: f64| -> Result<Res<Response>> {
        r.status.set(status_of(status)?);
        Ok(r)
    })
    .name("hari", "상태설정")
    .name("kanade", "状態設定");
    resp.method("set_header", |r: Res<Response>, name: Str, value: Str| {
        r.set_header(&name, &value);
        r
    })
    .name("hari", "헤더설정")
    .name("kanade", "ヘッダー設定");
    resp.method("set_cookie", |r: Res<Response>, name: Str, value: Str, options: Option<Dict>| {
        let line = set_cookie(&name, &value, &cookie_options(options));
        r.headers.borrow_mut().push(("Set-Cookie".into(), line));
        r
    })
    .name("hari", "쿠키설정")
    .name("kanade", "クッキー設定");
    resp.method("delete_cookie", |r: Res<Response>, name: Str| {
        let o = CookieOptions { max_age: Some(0), expires: Some("Thu, 01 Jan 1970 00:00:00 GMT".into()), ..Default::default() };
        r.headers.borrow_mut().push(("Set-Cookie".into(), set_cookie(&name, "", &o)));
        r
    })
    .name("hari", "쿠키삭제")
    .name("kanade", "クッキー削除");
}

// ---------------------------------------------------------------------------
// Serving a request (on the program's thread)

thread_local! {
    /// What `<중단>` asked for, until the handler's failure is seen.
    static ABORT: RefCell<Option<(u16, Option<String>)>> = const { RefCell::new(None) };
    /// The template folder of the app serving a request now.
    static TEMPLATES: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static TEMPLATE_CACHE: RefCell<HashMap<PathBuf, (Option<SystemTime>, Rc<template::Template>)>> = RefCell::new(HashMap::new());
}

fn abort(status: u16, message: Option<String>) -> Error {
    ABORT.with(|a| *a.borrow_mut() = Some((status, message)));
    Error::new("Abort").arg(status as f64)
}

/// Why a request did not get its response.
enum Failure {
    /// An HTTP error (`<중단>`, no such page): its status, a message, and
    /// headers it comes with (`Allow`).
    Http(u16, Option<String>, Vec<(String, String)>),
    /// The program failed: its message.
    Error(String),
}

/// Calls a function of the program with as many of the arguments as it takes.
fn call_user(f: &Value, args: &[Value], lang: Lang) -> std::result::Result<Value, Failure> {
    let n = f.param_count().map_or(args.len(), |n| n.min(args.len()));
    ABORT.with(|a| a.borrow_mut().take());
    f.call_catching(&args[..n], lang.locale()).map_err(|message| match ABORT.with(|a| a.borrow_mut().take()) {
        Some((status, m)) => Failure::Http(status, m, Vec::new()),
        None => Failure::Error(message),
    })
}

/// What a handler returned, as a response.
fn into_response(v: &Value, lang: Lang) -> std::result::Result<Res<Response>, Failure> {
    if let Some(r) = v.as_res::<Response>() {
        return Ok(r);
    }
    if v.is_null() {
        return Res::new(Response::new(204, None, Vec::new())).map_err(|_| Failure::Error(String::new()));
    }
    match body_of(v) {
        Ok((ctype, bytes)) => Res::new(Response::new(200, ctype, bytes)).map_err(|_| Failure::Error(String::new())),
        Err(_) => {
            let t = from_value(v, lang).text(lang);
            Err(Failure::Error(lang.tr(
                &format!("처리기는 글, 사전, 목록, 응답, 비어있음 중 하나를 돌려줘야 해요 (돌려준 값: {t})"),
                &format!("処理は文字列・辞書・リスト・応答・空っぽのどれかを返します（返した値: {t}）"),
            )))
        }
    }
}

fn dispatch(app: &Res<App>, inc: Incoming) -> Result<Res<Response>> {
    let req = Res::new(Request::new(app, inc))?;
    let reqv = req.value().clone();
    let folder = app.templates.borrow().clone();
    let outer = TEMPLATES.with(|t| t.replace(Some(folder)));
    let result = serve(app, &req, &reqv);
    TEMPLATES.with(|t| *t.borrow_mut() = outer);
    let resp = result?;
    if req.method == "HEAD" {
        resp.body.borrow_mut().clear();
    }
    Ok(resp)
}

fn serve(app: &Res<App>, req: &Res<Request>, reqv: &Value) -> Result<Res<Response>> {
    let lang = app.lang;
    let mut resp = match route(app, req, reqv) {
        Ok(r) => r,
        Err(f) => failure_response(app, reqv, f)?,
    };
    let hooks = app.after.borrow().clone();
    for h in hooks {
        match call_user(&h, &[reqv.clone(), resp.value().clone()], lang) {
            Ok(v) => {
                if let Some(r) = v.as_res::<Response>() {
                    resp = r;
                }
            }
            Err(f) => {
                resp = failure_response(app, reqv, f)?;
                break;
            }
        }
    }
    save_session(req, &resp);
    Ok(resp)
}

fn route(app: &Res<App>, req: &Res<Request>, reqv: &Value) -> std::result::Result<Res<Response>, Failure> {
    let lang = app.lang;
    let hooks = app.before.borrow().clone();
    for h in hooks {
        let v = call_user(&h, &[reqv.clone()], lang)?;
        if !v.is_null() {
            return into_response(&v, lang);
        }
    }
    let routes = app.routes.borrow().clone();
    let mut best: Option<(Vec<u8>, Rc<Route>, Vec<(String, V)>)> = None;
    let mut allowed: Vec<String> = Vec::new();
    let mut needs_slash = false;
    for r in &routes {
        match r.rule.matches(&req.path) {
            Match::Yes(vars) if r.accepts(&req.method) => {
                let w = r.rule.weight();
                if best.as_ref().is_none_or(|(bw, _, _)| w < *bw) {
                    best = Some((w, r.clone(), vars));
                }
            }
            Match::Yes(_) => allowed.extend(r.methods.iter().cloned()),
            Match::NeedsSlash => needs_slash = true,
            Match::No => {}
        }
    }
    if let Some((_, route, vars)) = best {
        *req.params.borrow_mut() = vars;
        let v = call_user(&route.handler, &[reqv.clone()], lang)?;
        return into_response(&v, lang);
    }
    let new = |status: u16| Res::new(Response::new(status, None, Vec::new())).map_err(|_| Failure::Error(String::new()));
    if !allowed.is_empty() {
        if allowed.iter().any(|m| m == "GET") {
            allowed.push("HEAD".into());
        }
        allowed.push("OPTIONS".into());
        allowed.sort();
        allowed.dedup();
        let allow = vec![("Allow".to_string(), allowed.join(", "))];
        if req.method == "OPTIONS" {
            let r = new(200)?;
            r.set_header("Allow", &allow[0].1);
            return Ok(r);
        }
        return Err(Failure::Http(405, None, allow));
    }
    if needs_slash {
        let mut location = format!("{}/", req.raw_path);
        if !req.query_string.is_empty() {
            location = format!("{location}?{}", req.query_string);
        }
        let r = new(308)?;
        r.set_header("Location", &location);
        return Ok(r);
    }
    if req.method == "GET" || req.method == "HEAD" {
        let statics = app.statics.borrow().clone();
        for (prefix, folder) in statics {
            let Some(rel) = req.path.strip_prefix(&prefix).and_then(|r| r.strip_prefix('/')) else { continue };
            if let Some(file) = safe_join(&folder, rel) {
                if file.is_file() {
                    return send_file(&file, None).map_err(|_| Failure::Http(404, None, Vec::new()));
                }
            }
        }
    }
    Err(Failure::Http(404, None, Vec::new()))
}

/// A file under a folder, never outside it (no `..`, no absolute path).
fn safe_join(folder: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    if rel.as_os_str().is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) || rel.to_string_lossy().contains('\\') {
        return None;
    }
    Some(folder.join(rel))
}

fn send_file(path: &Path, content_type: Option<&str>) -> Result<Res<Response>> {
    let Ok(bytes) = std::fs::read(path) else { return Err(abort(404, None)) };
    let r = Response::new(200, Some(content_type.unwrap_or_else(|| mime_for(path))), bytes);
    if let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) {
        if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
            r.set_header("Last-Modified", &util::http_date(d.as_secs() as i64));
        }
    }
    Res::new(r)
}

/// The response to a failure: the app's error handler for the status, or a
/// plain page.
fn failure_response(app: &Res<App>, reqv: &Value, f: Failure) -> Result<Res<Response>> {
    let lang = app.lang;
    let (status, message, headers, error) = match f {
        Failure::Http(s, m, h) => (s, m, h, None),
        Failure::Error(e) => (500, None, Vec::new(), Some(e)),
    };
    if let Some(e) = &error {
        haru_sdk::flush_output();
        eprintln!("{}", lang.tr(&format!("[하늘] 처리 중 오류: {e}"), &format!("[空] 処理中のエラー: {e}")));
    }
    let handler = app.errors.borrow().iter().find(|(s, _)| *s == status).map(|(_, h)| h.clone());
    if let Some(h) = handler {
        let detail = message.clone().or_else(|| error.clone()).unwrap_or_else(|| describe(lang, status));
        match call_user(&h, &[reqv.clone(), Value::str(&detail)], lang) {
            Ok(v) => {
                let from_value_itself = v.as_res::<Response>().is_some();
                if let Ok(r) = into_response(&v, lang) {
                    // A page returned as it is gets the error's status.
                    if !from_value_itself {
                        r.status.set(status);
                    }
                    for (k, val) in &headers {
                        if r.header(k).is_none() {
                            r.set_header(k, val);
                        }
                    }
                    return Ok(r);
                }
            }
            Err(Failure::Error(e)) => {
                haru_sdk::flush_output();
                eprintln!("{}", lang.tr(&format!("[하늘] 오류 처리기의 오류: {e}"), &format!("[空] エラー処理のエラー: {e}")));
            }
            Err(Failure::Http(..)) => {}
        }
        return error_page(app, 500, None, None, Vec::new());
    }
    error_page(app, status, message, error, headers)
}

/// What an error status means, in a sentence (the status's name if none).
fn describe(lang: Lang, status: u16) -> String {
    let text = match status {
        400 => lang.tr("요청을 이해할 수 없어요.", "リクエストを理解できません。"),
        401 => lang.tr("로그인이 필요해요.", "ログインが必要です。"),
        403 => lang.tr("이 주소에 접근할 수 없어요.", "このアドレスにはアクセスできません。"),
        404 => lang.tr("요청한 주소를 찾을 수 없어요.", "お探しのページが見つかりません。"),
        405 => lang.tr("이 주소는 이 메서드를 받지 않아요.", "このアドレスはこのメソッドを受け付けません。"),
        500 => lang.tr("서버에서 오류가 났어요.", "サーバーでエラーが起きました。"),
        _ => String::new(),
    };
    if text.is_empty() {
        status_text(status).to_string()
    } else {
        text
    }
}

fn error_page(app: &App, status: u16, message: Option<String>, error: Option<String>, headers: Vec<(String, String)>) -> Result<Res<Response>> {
    let lang = app.lang;
    let text = message.unwrap_or_else(|| describe(lang, status));
    let title = status_text(status);
    let mut page = format!(
        "<!doctype html>\n<html lang=\"{}\">\n<title>{status} {title}</title>\n<h1>{title}</h1>\n",
        if lang == Lang::Hari { "ko" } else { "ja" }
    );
    if !text.is_empty() {
        page.push_str(&format!("<p>{}</p>\n", html_escape(&text)));
    }
    if let (true, Some(e)) = (app.debug.get(), error) {
        page.push_str(&format!("<pre>{}</pre>\n", html_escape(&e)));
    }
    let r = Response::new(status, Some("text/html; charset=utf-8"), page.into_bytes());
    for (k, v) in headers {
        r.set_header(&k, &v);
    }
    Res::new(r)
}

/// Writes the session back into its cookie when it changed.
fn save_session(req: &Request, resp: &Response) {
    let Some((value, before)) = req.session.borrow().clone() else { return };
    let Some(key) = req.secret.clone() else { return };
    let now = match to_json(&from_value(&value, req.lang), false) {
        Ok(t) => t,
        Err(what) => {
            eprintln!("{}", req.lang.tr(&format!("[하늘] 세션에 JSON으로 바꿀 수 없는 값이 있어요: {what}"), &format!("[空] セッションにJSONにできない値があります: {what}")));
            return;
        }
    };
    resp.set_header("Vary", "Cookie");
    if now == before {
        return;
    }
    let base = CookieOptions { http_only: true, same_site: Some("Lax".into()), ..Default::default() };
    let line = if now == "{}" {
        set_cookie(SESSION_COOKIE, "", &CookieOptions { max_age: Some(0), expires: Some("Thu, 01 Jan 1970 00:00:00 GMT".into()), ..base })
    } else {
        set_cookie(SESSION_COOKIE, &sign(&key, &now), &base)
    };
    resp.headers.borrow_mut().push(("Set-Cookie".into(), line));
}

// ---------------------------------------------------------------------------
// Running

fn run(app: Res<App>, port: Option<f64>, host: Option<Str>) -> Result<()> {
    if app.running.borrow().is_some() {
        return Err(Error::new("AlreadyRunning"));
    }
    let port = port.unwrap_or(5000.0);
    if port.fract() != 0.0 || !(0.0..=65535.0).contains(&port) {
        return Err(Error::new("BadPort").arg(port));
    }
    let host = host.map_or_else(|| "127.0.0.1".to_string(), |h| h.to_string());
    let listener =
        http::bind(&host, port as u16).map_err(|e| Error::new("Listen").arg(format!("{host}:{port}")).arg(e.to_string()))?;
    let local = listener.local_addr().ok();
    let stop = Arc::new(AtomicBool::new(false));
    let (jobs, incoming) = channel::<Job>();
    {
        let stop = stop.clone();
        std::thread::spawn(move || http::accept_loop(listener, jobs, stop));
    }
    *app.running.borrow_mut() = Some(stop.clone());
    let shown = match local {
        Some(a) if a.is_ipv6() => format!("http://[{}]:{}", a.ip(), a.port()),
        Some(a) => format!("http://{}:{}", a.ip(), a.port()),
        None => format!("http://{host}:{port}"),
    };
    haru_sdk::flush_output();
    if app.log.get() {
        eprintln!(
            "{}",
            app.lang.tr(
                &format!(" * 하늘 앱이 {shown} 에서 요청을 기다려요 (멈추려면 Ctrl+C)"),
                &format!(" * 空のアプリが {shown} で待っています（止めるには Ctrl+C）")
            )
        );
    }
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match incoming.recv_timeout(Duration::from_millis(50)) {
            Ok(job) => {
                let inc = job.incoming;
                let line = format!("{} {} {}", inc.method, inc.target, inc.version);
                let ip = inc.remote.parse::<std::net::SocketAddr>().map_or_else(|_| inc.remote.clone(), |a| a.ip().to_string());
                let answer = match dispatch(&app, inc) {
                    Ok(r) => r.answer(),
                    Err(_) => Answer { status: 500, headers: Vec::new(), body: Vec::new() },
                };
                // What the handler printed shows before the log line.
                haru_sdk::flush_output();
                if app.log.get() {
                    eprintln!("{ip} - - [{}] \"{line}\" {} -", log_date(now_secs()), answer.status);
                }
                let _ = job.reply.send(answer);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    *app.running.borrow_mut() = None;
    if let Some(a) = local {
        http::wake(a);
    }
    Ok(())
}

/// `'앱'의 <시험요청>("GET", "/경로", 본문?, 헤더?)`: the app answers a
/// request made here, without the network (for tests). A dictionary or list
/// body goes as JSON. Cookies the app sets are sent with later test requests.
fn test_request(app: Res<App>, method: Str, target: Str, body: Option<Value>, headers: Option<Dict>) -> Result<Res<Response>> {
    if !target.starts_with('/') {
        return Err(Error::new("BadTarget").arg(&*target));
    }
    let mut hs: Vec<(String, String)> = Vec::new();
    if let Some(h) = headers {
        for k in h.keys().iter() {
            let v = h.get(&k).unwrap_or(Value::NULL);
            hs.push((from_value(&k, app.lang).text(app.lang), from_value(&v, app.lang).text(app.lang)));
        }
    }
    let has = |hs: &[(String, String)], name: &str| hs.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    let body = match body {
        None => Vec::new(),
        Some(v) => match v.tag() {
            tag::NULL => Vec::new(),
            tag::STR => v.as_str().unwrap().as_bytes().to_vec(),
            tag::LIST | tag::DICT => {
                if !has(&hs, "Content-Type") {
                    hs.push(("Content-Type".into(), "application/json".into()));
                }
                json_text(&v)?.into_bytes()
            }
            _ => return Err(Error::new("BadBody").arg(from_value(&v, app.lang).text(app.lang))),
        },
    };
    if !body.is_empty() && !has(&hs, "Content-Length") {
        hs.push(("Content-Length".into(), body.len().to_string()));
    }
    if !has(&hs, "Cookie") {
        let jar = app.test_cookies.borrow();
        if !jar.is_empty() {
            let line: Vec<String> = jar.iter().map(|(k, v)| format!("{k}={}", util::percent_encode(v))).collect();
            hs.push(("Cookie".into(), line.join("; ")));
        }
    }
    let inc = Incoming {
        method: method.to_ascii_uppercase(),
        target: target.to_string(),
        version: "HTTP/1.1".into(),
        headers: hs,
        body,
        remote: "127.0.0.1:0".into(),
    };
    let resp = dispatch(&app, inc)?;
    // Keep what the app set, as a browser would.
    let set: Vec<String> = resp.headers.borrow().iter().filter(|(k, _)| k.eq_ignore_ascii_case("Set-Cookie")).map(|(_, v)| v.clone()).collect();
    let mut jar = app.test_cookies.borrow_mut();
    for line in set {
        let first_part = line.split(';').next().unwrap_or("");
        let Some((k, v)) = first_part.split_once('=') else { continue };
        let expired = line.split(';').any(|p| p.trim().eq_ignore_ascii_case("Max-Age=0"));
        jar.retain(|(name, _)| name != k.trim());
        if !expired {
            jar.push((k.trim().to_string(), percent_decode(v.trim(), false)));
        }
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Templates

fn template_error(message: String) -> Error {
    Error::new("Template").arg(message)
}

fn template_folder() -> PathBuf {
    TEMPLATES.with(|t| t.borrow().clone()).unwrap_or_else(|| PathBuf::from("templates"))
}

fn load_template(folder: &Path, name: &str, lang: Lang) -> std::result::Result<Rc<template::Template>, String> {
    let not_found = || lang.tr(&format!("템플릿을 찾을 수 없어요: {name}"), &format!("テンプレートが見つかりません: {name}"));
    let path = safe_join(folder, name).ok_or_else(not_found)?;
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
    if let Some((when, t)) = TEMPLATE_CACHE.with(|c| c.borrow().get(&path).cloned()) {
        if when.is_some() && when == modified {
            return Ok(t);
        }
    }
    let src = std::fs::read_to_string(&path).map_err(|_| not_found())?;
    let t = Rc::new(template::parse(&src, name, lang)?);
    TEMPLATE_CACHE.with(|c| c.borrow_mut().insert(path, (modified, t.clone())));
    Ok(t)
}

fn vars_of(vars: Option<Dict>, lang: Lang) -> Result<V> {
    Ok(match vars {
        Some(d) => from_value(&d.into_ret()?, lang),
        None => V::Null,
    })
}

fn render_file(name: &str, vars: Option<Dict>, lang: Lang) -> Result<String> {
    let vars = vars_of(vars, lang)?;
    let folder = template_folder();
    let mut loader = |n: &str| load_template(&folder, n, lang);
    let t = loader(name).map_err(template_error)?;
    template::render(&t, &vars, lang, &mut loader).map_err(template_error)
}

fn render_text(text: &str, vars: Option<Dict>, lang: Lang) -> Result<String> {
    let vars = vars_of(vars, lang)?;
    let folder = template_folder();
    let name = lang.tr("<템플릿문자열>", "<テンプレート文字列>");
    let t = Rc::new(template::parse(text, &name, lang).map_err(template_error)?);
    let mut loader = |n: &str| load_template(&folder, n, lang);
    template::render(&t, &vars, lang, &mut loader).map_err(template_error)
}
