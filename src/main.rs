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
use structopt::StructOpt;
use uuid::Uuid;

use transfer_rs::transfer_rs::prelude::*;

type BoxFut = Box<dyn Future<Item = Response<Body>, Error = hyper::Error> + Send>;

#[derive(StructOpt)]
#[structopt(name = "fileserver")]
struct Opt {
    #[structopt(short, long)]
    /// Port number
    port: i32,
}

struct MultipartRegexs {
    boundary: Regex,
    form_data: Regex,
    mime: Regex,
    content_disposition_name: Regex,
    content_disposition_filename: Regex,
}

fn main() -> Fallible<()> {
    dotenv::dotenv().ok();
    env_logger::init();
    info!("Hello");

    let opt: Opt = Opt::from_args();
    info!("port: {}", opt.port);

    std::fs::create_dir_all("data")?;

    let multipart_regexs = Arc::new(create_multipart_regexs()?);

    hyper::rt::run(
        Server::bind(&format!("0.0.0.0:{}", opt.port).parse()?)
            .serve(move || {
                info!("new service");
                let multipart_regexs = multipart_regexs.clone();
                service::service_fn(move |req| {
                    let multipart_regexs = multipart_regexs.clone();
                    info!("uri: {:?}", req.uri());
                    info!("version: {:?}", req.version());
                    info!("headers: {:?}", req.headers());
                    info!("method: {:?}", req.method());

                    match req.method() {
                        &Method::GET
                        | &Method::PUT
                        | &Method::DELETE
                        | &Method::HEAD
                        | &Method::OPTIONS
                        | &Method::CONNECT
                        | &Method::PATCH
                        | &Method::TRACE => return handler_not_implemented(),
                        _ => (),
                    }

                    match req.uri().path() {
                        "/upload" => {
                            if req.method() == &Method::POST {
                                upload_handler(req, multipart_regexs)
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
    file_writer: Option<BufWriter<std::fs::File>>,
    buffer: Vec<u8>,
    regexs: Arc<MultipartRegexs>,
    body_skip_crlf: bool,
    file_root: PathBuf,
}

impl ParseMultipartContext {
    fn new(boundary: String, regexs: Arc<MultipartRegexs>, file_root: PathBuf) -> Self {
        Self {
            boundary,
            command: ParseType::LoadBoundary,
            name: Default::default(),
            file_uuid: Default::default(),
            filename: Default::default(),
            file_writer: Default::default(),
            buffer: Default::default(),
            regexs,
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
                        let reg_formdata = &context.regexs.form_data;
                        let reg_mime = &context.regexs.mime;
                        if reg_formdata.is_match(&s) {
                            info!("ContentDescription: '{}'", s);
                            match context.regexs.content_disposition_name.captures(&s) {
                                Some(name) => match name.get(1) {
                                    Some(name) => context.name = Some(name.as_str().to_owned()),
                                    None => return Err(format_err!("unexpected")),
                                },
                                None => (),
                            }
                            match context.regexs.content_disposition_filename.captures(&s) {
                                Some(filename) => match filename.get(1) {
                                    Some(filename) => {
                                        context.file_uuid = Some(Uuid::new_v4());
                                        context.filename = Some(filename.as_str().to_owned());
                                    }
                                    None => return Err(format_err!("unexpected")),
                                },
                                None => (),
                            }
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
            ParseType::End => Ok(CommandRet::Consumed),
        }
    }
}

fn upload_handler(req: Request<Body>, multipart_regexs: Arc<MultipartRegexs>) -> BoxFut {
    let file_root = "data";
    if let Some(val) = req.headers().get(hyper::header::CONTENT_TYPE) {
        if let Ok(a) = val.to_str() {
            if a.to_lowercase().contains("multipart/form-data") {
                let reg = &multipart_regexs.boundary;

                let boundary = match reg.captures(a) {
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
                return Box::new(
                    req.into_body()
                        .fold(
                            ParseMultipartContext::new(
                                boundary,
                                multipart_regexs.clone(),
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
                                    match &context
                                        .command
                                        .clone()
                                        .execute(&mut context, &mut reader)
                                    {
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
                        .map(|context| {
                            if context.command == ParseType::End {
                                info!("success end");
                            } else {
                                warn!(
                                    "all data received but unexpected state: {:?}",
                                    context.command
                                );
                            }
                            Response::builder()
                                .status(StatusCode::INTERNAL_SERVER_ERROR)
                                .body(Body::from("TODO"))
                                .unwrap()
                        }),
                );
                //                let body = req.into_body().concat2();
                //                return Box::new(body.map(|data| {
                //                    std::fs::write("data/aa", data).ok();
                //                    Response::builder()
                //                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                //                        .body(Body::from("TODO"))
                //                        .unwrap()
                //                }));
            }
        }
    }

    let (head, body) = req.into_parts();
    let filename = match head.headers.get("x-tp-filename") {
        Some(filename) => match filename.to_str() {
            Ok(filename) => filename.to_owned(),
            _ => "a".to_owned(),
        },
        None => "a".to_owned(),
    };
    let body = body.concat2();
    Box::new(body.map(move |data| {
        let file_id = Uuid::new_v4();
        let filepath = PathBuf::new()
            .join(file_root)
            .join(format!("{}", file_id))
            .join(filename);
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
        match std::fs::write(filepath, data) {
            Ok(_) => {
                info!("wrote");
                Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::from("ok"))
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

fn create_multipart_regexs() -> Fallible<MultipartRegexs> {
    let boundary = Regex::new("boundary=([^;]*)")?;
    let form_data = Regex::new("^Content-Disposition: form-data(;|$)")?;
    let mime = Regex::new("Content-Type: (.*)$")?;
    let content_disposition_name = Regex::new(r#"^Content-Disposition:.* name="([^"]*)"(;|\r\n)"#)?;
    let content_disposition_filename =
        Regex::new(r#"^Content-Disposition:.* filename="([^"]*)"(;|\r\n)"#)?;
    Ok(MultipartRegexs {
        boundary,
        form_data,
        mime,
        content_disposition_name,
        content_disposition_filename,
    })
}
