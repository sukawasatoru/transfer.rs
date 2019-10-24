/*
 * Copyright 2019 sukawasatoru
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::io::{prelude::*, BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::Arc;

use failure::format_err;
use futures::{future, Future, Stream};
use hyper::{service, Body, Method, Request, Response, Server, StatusCode};
use log::{debug, info, warn};
use regex::Regex;
use serde_derive::Serialize;
use structopt::StructOpt;
use uuid::Uuid;

use transfer_rs::transfer_rs::prelude::*;

type BoxFut = Box<dyn Future<Item = Response<Body>, Error = hyper::Error> + Send>;

#[derive(StructOpt)]
#[structopt(name = "transfer")]
struct Opt {
    #[structopt(short, long)]
    /// Port number
    port: i32,
}

struct MultipartRegexps {
    boundary: Regex,
    form_data: Regex,
    mime: Regex,
    content_disposition_name: Regex,
    content_disposition_filename: Regex,
}

#[derive(Serialize)]
struct UploadResult {
    part: Vec<UploadResultPart>,
    error: Option<String>,
}

#[derive(Serialize)]
struct UploadResultPart {
    name: String,
    file_name: String,
    url: String,
    error: Option<String>,
}

fn main() -> Fallible<()> {
    dotenv::dotenv().ok();
    env_logger::init();
    info!("Hello");

    let opt: Opt = Opt::from_args();
    info!("port: {}", opt.port);

    std::fs::create_dir_all("data")?;

    let multipart_regexps = Arc::new(create_multipart_regexps()?);

    hyper::rt::run(
        Server::bind(&format!("0.0.0.0:{}", opt.port).parse()?)
            .serve(move || {
                info!("new service");
                let multipart_regexps = multipart_regexps.clone();
                service::service_fn(move |req| {
                    let multipart_regexps = multipart_regexps.clone();
                    info!("uri: {:?}", req.uri());
                    info!("version: {:?}", req.version());
                    info!("headers: {:?}", req.headers());
                    info!("method: {:?}", req.method());

                    match *req.method() {
                        Method::PUT
                        | Method::DELETE
                        | Method::HEAD
                        | Method::OPTIONS
                        | Method::CONNECT
                        | Method::PATCH
                        | Method::TRACE => return handler_not_implemented(),
                        _ => (),
                    }

                    // TODO: sanitize path. e.g. http://host/../filename.jpg
                    let get_path_regexp = Regex::new(&format!(r#"^/([^/]*)/([^/]*)$"#)).unwrap();
                    if *req.method() == Method::GET && get_path_regexp.is_match(req.uri().path()) {
                        return get_handler();
                    }

                    match req.uri().path() {
                        "/upload" => {
                            if *req.method() == Method::POST {
                                upload_handler(req, multipart_regexps)
                            } else {
                                handler_method_not_allowed()
                            }
                        }
                        // path if path == "" => {}
                        _ => handler_not_found(),
                    }
                })
            })
            .map_err(|e| println!("server error: {}", e)),
    );

    info!("Bye");
    Ok(())
}

fn get_handler() -> BoxFut {
    Box::new(future::ok(
        Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Body::from("TODO get handler"))
            .unwrap(),
    ))
}

#[derive(Clone, Debug, PartialEq)]
enum ParseType {
    LoadBoundary,
    LoadContentDescription,
    Body,
    End,
}

#[derive(Debug, PartialEq)]
enum CommandRet {
    NextCommand,
    Consumed,
}

struct ParseMultipartContext {
    boundary: String,
    command: ParseType,
    name: Option<String>,
    file_uuid: Option<Uuid>,
    filename: Option<String>,
    processed: Vec<String>,
    file_writer: Option<BufWriter<std::fs::File>>,
    buffer: Vec<u8>,
    regexps: Arc<MultipartRegexps>,
    body_skip_crlf: bool,
    file_root: PathBuf,
}

impl ParseMultipartContext {
    fn new(boundary: String, regexps: Arc<MultipartRegexps>, file_root: PathBuf) -> Self {
        Self {
            boundary,
            command: ParseType::LoadBoundary,
            name: Default::default(),
            file_uuid: Default::default(),
            filename: Default::default(),
            processed: Default::default(),
            file_writer: Default::default(),
            buffer: Default::default(),
            regexps,
            body_skip_crlf: Default::default(),
            file_root,
        }
    }
}

trait ParseMultipartCommand {
    fn execute(
        &self,
        context: &mut ParseMultipartContext,
        reader: &mut BufReader<&[u8]>,
    ) -> Fallible<CommandRet>;
}

impl ParseMultipartCommand for ParseType {
    fn execute(
        &self,
        context: &mut ParseMultipartContext,
        reader: &mut BufReader<&[u8]>,
    ) -> Fallible<CommandRet> {
        match &self {
            ParseType::LoadBoundary => {
                let line = {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => return Ok(CommandRet::Consumed),
                        Ok(_) => {
                            if line.ends_with("\r\n") {
                                let mut ret_line = Vec::new();
                                ret_line.append(&mut context.buffer);
                                ret_line.extend(line.into_bytes());
                                ret_line
                            } else {
                                context.buffer.extend(line.into_bytes());
                                return Ok(CommandRet::NextCommand);
                            }
                        }
                        Err(e) => {
                            return Err(format_err!("failed to read line: {:?}", e));
                        }
                    }
                };

                match String::from_utf8(line) {
                    Ok(s) => {
                        info!("boundary: '{}'", context.boundary);
                        info!("s len: {}, val: '{}'", s.len(), s);
                        if s == format!("--{}\r\n", context.boundary) {
                            info!("boundary consumed");
                            context.command = ParseType::LoadContentDescription;
                            return Ok(CommandRet::NextCommand);
                        } else {
                            // TODO:
                            return Err(format_err!("failed to consume a boundary"));
                        }
                    }
                    Err(e) => {
                        // TODO:
                        return Err(format_err!("failed to parse boundary: {:?}", e));
                    }
                }
            }
            ParseType::LoadContentDescription => {
                let line = {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => {
                            info!("empty");
                            return Ok(CommandRet::Consumed);
                        }
                        Ok(_) => {
                            if line == "\r\n" && context.buffer.is_empty() {
                                context.command = ParseType::Body;
                                return Ok(CommandRet::NextCommand);
                            } else if line.ends_with("\r\n") {
                                let mut ret_val = Vec::new();
                                ret_val.append(&mut context.buffer);
                                ret_val.extend(line.into_bytes());
                                ret_val
                            } else {
                                context.buffer.extend(line.into_bytes());
                                return Ok(CommandRet::NextCommand);
                            }
                        }
                        Err(e) => {
                            return Err(format_err!("failed to read line: {:?}", e));
                        }
                    }
                };

                match String::from_utf8(line) {
                    Ok(s) => {
                        let reg_formdata = &context.regexps.form_data;
                        let reg_mime = &context.regexps.mime;
                        if reg_formdata.is_match(&s) {
                            info!("ContentDescription: '{}'", s);
                            match context.regexps.content_disposition_name.captures(&s) {
                                Some(name) => match name.get(1) {
                                    Some(name) => context.name = Some(name.as_str().to_owned()),
                                    None => return Err(format_err!("unexpected")),
                                },
                                None => (),
                            }
                            match context.regexps.content_disposition_filename.captures(&s) {
                                Some(filename) => match filename.get(1) {
                                    Some(filename) => {
                                        let mut uuid = Some(Uuid::new_v4());
                                        let mut filename = Some(filename.as_str().to_owned());
                                        std::mem::swap(&mut context.file_uuid, &mut uuid);
                                        std::mem::swap(&mut context.filename, &mut filename);
                                        if uuid.is_some() && filename.is_some() {
                                            context.processed.push(format!(
                                                "{}/{}",
                                                uuid.unwrap(),
                                                filename.unwrap()
                                            ));
                                        }
                                    }
                                    None => return Err(format_err!("unexpected")),
                                },
                                None => (),
                            }
                            info!("name: {:?}, filename: {:?}", context.name, context.filename);
                            return Ok(CommandRet::NextCommand);
                        } else if let Some(data) = reg_mime.captures(&s) {
                            match data.get(1) {
                                Some(data) => {
                                    info!("ContentDescription mime: '{}'", data.as_str());
                                    return Ok(CommandRet::NextCommand);
                                }
                                None => {
                                    // TODO:
                                    return Err(format_err!("unexpected"));
                                }
                            }
                        } else {
                            info!("ContentDescription (ignored): '{}'", s);
                            return Ok(CommandRet::NextCommand);
                        }
                    }
                    Err(e) => {
                        // TODO:
                        return Err(format_err!("failed to parse boundary: {:?}", e));
                    }
                }
            }
            ParseType::Body => {
                let mut line = {
                    let mut line = Vec::new();
                    match reader.read_until(b'\n', &mut line) {
                        Ok(0) => {
                            return Ok(CommandRet::Consumed);
                        }
                        Ok(_) => {
                            if line.ends_with(b"\r\n") {
                                if &line == b"\r\n" {
                                    info!("newline");
                                }
                                let mut ret_line = Vec::new();
                                ret_line.append(&mut context.buffer);
                                ret_line.extend(line);
                                ret_line
                            } else {
                                info!("body: next_buf.extend, chunk.len: {}", line.len());
                                context.buffer.extend(line);
                                return Ok(CommandRet::NextCommand);
                            }
                        }
                        Err(e) => {
                            return Err(format_err!("failed to read line: {:?}", e));
                        }
                    }
                };
                if line == format!("--{}\r\n", context.boundary).as_bytes() {
                    info!("match separator");
                    let mut writer = None;
                    std::mem::swap(&mut writer, &mut context.file_writer);
                    if let Some(mut writer) = writer {
                        writer.flush().ok();
                    }
                    context.command = ParseType::LoadContentDescription;
                    context.body_skip_crlf = false;
                    Ok(CommandRet::NextCommand)
                } else if line == format!("--{}--\r\n", context.boundary).as_bytes() {
                    info!("match end");
                    let mut writer = None;
                    std::mem::swap(&mut writer, &mut context.file_writer);
                    if let Some(mut writer) = writer {
                        writer.flush().ok();
                    }
                    context.command = ParseType::End;
                    Ok(CommandRet::NextCommand)
                } else {
                    info!("body.len: '{}'", line.len());
                    if context.body_skip_crlf {
                        line.insert(0, b'\r');
                        line.insert(1, b'\n');
                    }
                    if line.ends_with(b"\r\n") {
                        line.truncate(line.len() - 2);
                        context.body_skip_crlf = true;
                    }
                    context.body_skip_crlf = true;
                    let writer = match context.file_writer {
                        Some(ref mut writer) => writer,
                        None => {
                            let filename = context.filename.as_ref().unwrap();
                            let filepath = context
                                .file_root
                                .join(context.file_uuid.unwrap().to_string())
                                .join(filename);
                            let create_dir_ret =
                                std::fs::create_dir_all(filepath.parent().unwrap());
                            if let Err(e) = create_dir_ret {
                                return Err(format_err!("failed to create directory: {:?}", e));
                            }
                            context.file_writer = match std::fs::File::create(filepath) {
                                Ok(file) => Some(BufWriter::new(file)),
                                Err(e) => return Err(format_err!("failed to open file: {:?}", e)),
                            };
                            context.file_writer.as_mut().unwrap()
                        }
                    };
                    match writer.write_all(&line) {
                        Ok(_) => Ok(CommandRet::NextCommand),
                        Err(e) => return Err(format_err!("failed to write file: {:?}", e)),
                    }
                }
            }
            ParseType::End => {
                if context.file_uuid.is_some() && context.filename.is_some() {
                    context.processed.push(format!(
                        "{}/{}",
                        context.file_uuid.as_ref().unwrap(),
                        context.filename.as_ref().unwrap()
                    ));
                }
                Ok(CommandRet::Consumed)
            }
        }
    }
}

fn upload_handler(req: Request<Body>, multipart_regexps: Arc<MultipartRegexps>) -> BoxFut {
    if let Some(content_type) = req.headers().get(hyper::header::CONTENT_TYPE) {
        if let Ok(content_type) = content_type.to_str() {
            if content_type.contains("multipart/form-data") {
                // curl -F myfile=@$HOME/path/to/file
                return upload_handler_multipart(req, multipart_regexps);
            } else if content_type == "application/x-www-form-urlencoded" {
                info!("TODO: {}", content_type);
                // curl --data-urlencode name@file --data-urlencode name@file
                // name=<encoded>&name=<encoded>
                // curl --data-urlencode @file --data-urlencode @file
                // <encoded>&<encoded>
                return Box::new(future::ok(
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::from("TODO"))
                        .unwrap(),
                ));
            }
        }
    }

    // curl -H "Content-Type: application/octet-stream" --data-binary @$HOME/path/to/file
    // curl -H "Content-Type: image/png" --data-binary @$HOME/path/to/file
    // curl -H "Content-Type: foobar/baz" --data-binary @$HOME/path/to/file
    upload_handler_file(req)
}

fn upload_handler_file(req: Request<Body>) -> BoxFut {
    let file_root = "data";
    let (head, body) = req.into_parts();
    let filename = match head.headers.get("x-tp-filename") {
        Some(filename) => match filename.to_str() {
            Ok(filename) => filename.to_owned(),
            _ => "a".to_owned(),
        },
        None => "a".to_owned(),
    };
    let host = head
        .headers
        .get(hyper::header::HOST)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let body = body.concat2();
    Box::new(body.map(move |data| {
        let host = host;
        let file_id = Uuid::new_v4();
        let filepath = PathBuf::new()
            .join(file_root)
            .join(format!("{}", file_id))
            .join(&filename);
        match std::fs::create_dir_all(filepath.parent().unwrap()) {
            Ok(_) => (),
            Err(e) => {
                warn!("failed to create directory: {:?}", e);
                return Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("failed to create directory"))
                    .unwrap();
            }
        }
        match std::fs::write(&filepath, data) {
            Ok(_) => {
                info!("wrote");
                let upload_result = UploadResult {
                    part: vec![UploadResultPart {
                        name: "name".to_owned(),
                        file_name: filepath.file_name().unwrap().to_str().unwrap().to_owned(),
                        url: format!("http://{}/{}/{}", host, file_id, filename),
                        error: None,
                    }],
                    error: None,
                };
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(serde_json::to_string(&upload_result).unwrap()))
                    .unwrap()
            }
            Err(e) => {
                info!("err: {:?}", e);
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::empty())
                    .unwrap()
            }
        }
    }))
}

fn upload_handler_multipart(
    req: Request<Body>,
    multipart_regexps: Arc<MultipartRegexps>,
) -> BoxFut {
    let file_root = "data";
    let reg = &multipart_regexps.boundary;

    let content_type = match req.headers().get(hyper::header::CONTENT_TYPE) {
        Some(data) => match data.to_str() {
            Ok(data) => data,
            Err(e) => panic!("TODO"),
        },
        None => unreachable!(),
    };

    let boundary = match reg.captures(content_type) {
        Some(cap) => match cap.get(1) {
            Some(boundary) => boundary.as_str().to_owned(),
            None => {
                return Box::new(future::ok(
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(Body::from("failed to parse boundary"))
                        .unwrap(),
                ));
            }
        },
        None => {
            return Box::new(future::ok(
                Response::builder()
                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                    .body(Body::from("failed to capture"))
                    .unwrap(),
            ));
        }
    };
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    Box::new(
        req.into_body()
            .fold(
                ParseMultipartContext::new(
                    boundary,
                    multipart_regexps.clone(),
                    PathBuf::new().join(file_root),
                ),
                move |mut context, data| {
                    debug!("chunk size: {}", data.len());
                    let mut buf = Vec::new();
                    std::mem::swap(&mut context.buffer, &mut buf);
                    buf.extend(data);
                    let mut reader = BufReader::new(buf.as_slice());

                    if context.command == ParseType::End {
                        warn!("parsetype is end but received chunk");
                        return future::ok::<_, hyper::Error>(context);
                    }

                    loop {
                        match &context.command.clone().execute(&mut context, &mut reader) {
                            Ok(CommandRet::NextCommand) => (),
                            Ok(CommandRet::Consumed) => break,
                            Err(e) => {
                                // TODO:
                                warn!("{:?}", e);
                            }
                        }
                    }
                    return future::ok::<_, hyper::Error>(context);
                },
            )
            .map(move |context| {
                if context.command == ParseType::End {
                    info!("success end");
                } else {
                    warn!(
                        "all data received but unexpected state: {:?}",
                        context.command
                    );
                }
                let upload_result = UploadResult {
                    part: context
                        .processed
                        .iter()
                        .map(|data| UploadResultPart {
                            name: "name".to_owned(),
                            file_name: "file_name".to_owned(),
                            url: format!("http://{}/{}", host, data),
                            error: None,
                        })
                        .collect(),
                    error: None,
                };
                let body = serde_json::to_string(&upload_result).unwrap();
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from(body))
                    .unwrap()
            }),
    )
}

fn handler_not_implemented() -> BoxFut {
    Box::new(future::ok(
        Response::builder()
            .status(StatusCode::NOT_IMPLEMENTED)
            .body(Body::empty())
            .unwrap(),
    ))
}

fn handler_method_not_allowed() -> BoxFut {
    Box::new(future::ok(
        Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Body::empty())
            .unwrap(),
    ))
}

fn handler_not_found() -> BoxFut {
    Box::new(future::ok(
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap(),
    ))
}

fn create_multipart_regexps() -> Fallible<MultipartRegexps> {
    let boundary = Regex::new("boundary=([^;]*)")?;
    let form_data = Regex::new("^Content-Disposition: form-data(;|$)")?;
    let mime = Regex::new("Content-Type: (.*)$")?;
    let content_disposition_name = Regex::new(r#"^Content-Disposition:.* name="([^"]*)"(;|\r\n)"#)?;
    let content_disposition_filename =
        Regex::new(r#"^Content-Disposition:.* filename="([^"]*)"(;|\r\n)"#)?;
    Ok(MultipartRegexps {
        boundary,
        form_data,
        mime,
        content_disposition_name,
        content_disposition_filename,
    })
}
