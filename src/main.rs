use std::{sync::Arc, collections::{HashMap, hash_map::DefaultHasher}, fs::{OpenOptions, File, self}, path::Path, io::Write, process::exit, convert::Infallible, net::SocketAddr, hash::{Hash, Hasher}, time::Duration};

use futures::{executor::block_on, StreamExt, SinkExt};
use prost::{bytes::Bytes, Message};
use serde::{Deserialize, de::DeserializeOwned, Serialize};
use tokio::{sync::{mpsc::{self, channel}, Mutex}, time};
use warp::{Filter, filters::{ws::{Ws, WebSocket, self}, cors::Cors}, reply::Reply, reject::Rejection};

pub mod server {
    include!(concat!(env!("OUT_DIR"), "/server.rs"));
}

struct Server {
    event_stream_senders: Mutex<HashMap<String, mpsc::Sender<server::ServerMessage>>>,
    ids: Mutex<HashMap<String, IDSInfo>>,
    message_queue: Mutex<HashMap<String, Vec<String>>>,
    accounts: Mutex<HashMap<String, u64>>,
}

impl Server {
    pub fn new() -> Self {
        Server {
            event_stream_senders: Mutex::new(HashMap::new()),
            ids: Mutex::new(HashMap::new()),
            message_queue: Mutex::new(HashMap::new()),
            accounts: Mutex::new(HashMap::new())
        }
    }
}

#[derive(Deserialize)]
struct IDSPost {
    email: String,
    password: String,
    public_key: String,
}

#[derive(Deserialize, Serialize, Clone)]
struct IDSInfo {
    account: String,
    uuid: String,
    public_key: String,
}

#[derive(Deserialize)]
struct Account {
    email: String,
    password: String,
}

fn create_or_open_file(path: &str) -> Result<std::fs::File, std::io::Error> {
    return OpenOptions::new()
        .write(true)
        .create(!Path::new(path).exists())
        .truncate(true)
        .open(path);
}

fn cors() -> Cors {
    return warp::cors()
    .allow_any_origin()
    .allow_headers(vec!["User-Agent", "Sec-Fetch-Mode", "Referer", "Origin", "Access-Control-Request-Method", "Access-Control-Request-Headers", "Content-Type"])
    .allow_methods(vec!["POST", "GET"])
    .build();
}

#[tokio::main(flavor = "current_thread")]
pub async fn main() {
    let server_arc = if Path::new("accounts.json").exists() && Path::new("ids.json").exists() && Path::new("queue.json").exists() {
        let accounts_file = File::open(Path::new("accounts.json")).expect("eek!");
        let ids_file = File::open(Path::new("ids.json")).expect("eek!");
        let queue_file = File::open(Path::new("queue.json")).expect("eek!");
        let accounts: HashMap<String, u64> = serde_json::from_reader(accounts_file).expect("oof not json");
        let ids: HashMap<String, IDSInfo> = serde_json::from_reader(ids_file).expect("oof not json");
        let queue: HashMap<String, Vec<String>> = serde_json::from_reader(queue_file).expect("oof not json");
        Arc::new(Mutex::new(Server {accounts: Mutex::new(accounts), event_stream_senders: Mutex::new(HashMap::new()), ids: Mutex::new(ids), message_queue: Mutex::new(queue)}))
    } else {
        Arc::new(Mutex::new(Server::new()))
    };
    let save_server_arc = server_arc.clone();
    let save = || async move {
        let server = save_server_arc.lock().await;
        let accounts = server.accounts.lock().await;
        let accounts_json = serde_json::to_string(&*accounts);
        let accounts_file = create_or_open_file("accounts.json");
        if accounts_json.is_ok() && accounts_file.is_ok() {
            accounts_file.unwrap().write_all(accounts_json.as_ref().unwrap().as_bytes()).expect("eek!");
            println!("saved accounts successfully")
        } else {
            if accounts_json.is_err() {
                println!("{}", accounts_json.unwrap_err());
            }
            if accounts_file.is_err() {
                println!("{}", accounts_file.unwrap_err());
            }
        }
        let ids = server.ids.lock().await;
        let ids_json = serde_json::to_string(&*ids);
        let ids_file = create_or_open_file("ids.json");
        if ids_json.is_ok() && ids_file.is_ok() {
            ids_file.unwrap().write_all(ids_json.as_ref().unwrap().as_bytes()).expect("eek!");
            println!("saved ids successfully")
        } else {
            if ids_json.is_err() {
                println!("{}", ids_json.unwrap_err());
            }
            if ids_file.is_err() {
                println!("{}", ids_file.unwrap_err());
            }
        }
        let queue = server.message_queue.lock().await;
        let queue_json = serde_json::to_string(&*queue);
        let queue_file = create_or_open_file("queue.json");
        if queue_json.is_ok() && queue_file.is_ok() {
            queue_file.unwrap().write_all(queue_json.as_ref().unwrap().as_bytes()).expect("eek!");
            println!("saved queue successfully");
        } else {
            if queue_json.is_err() {
                println!("{}", queue_json.unwrap_err());
            }
            if queue_file.is_err() {
                println!("{}", queue_file.unwrap_err());
            }
        }
    };
    let ctrlc_save = save.clone();
    ctrlc::set_handler(move || {
        println!("received Ctrl+C!");
        block_on(ctrlc_save.clone()());
        exit(1);
    })
    .expect("Error setting Ctrl-C handler");
    let routes = warp::path!("events" / String)
        // The `ws()` filter will prepare the Websocket handshake.
        .and(warp::ws())
        .and(with_server(server_arc.clone()))
        .and_then(websocket)
        .with(cors())
        .or(
            warp::path!("register-ids")
            .and(json_body())
            .and(with_server(server_arc.clone()))
            .and_then(register_ids)
            .with(cors())
        )
        .with(cors())
        .or(
            warp::path!("create-account")
            .and(json_body())
            .and(with_server(server_arc.clone()))
            .and_then(create_account)
            .with(cors())
        )
        .with(cors())
        .or(
            warp::path!("query-ids" / String)
            .and(with_server(server_arc.clone()))
            .and_then(query_ids)
            .with(cors())
        )
        .with(cors())
        .or(
            warp::path!("post-message")
            .and(body_bytes())
            .and(with_server(server_arc.clone()))
            .and_then(post_message)
            .with(cors())
        );

    let addr: SocketAddr = "0.0.0.0:40441".parse().unwrap();

    println!("starting warp server");
    warp::serve(routes).tls()
    .cert_path("/etc/letsencrypt/live/chrissytopher.com/fullchain.pem")
    .key_path("/etc/letsencrypt/live/chrissytopher.com/privkey.pem")
    .run(addr).await;
    save().await;
}

fn with_server(server: Arc<Mutex<Server>>) -> impl Filter<Extract = (Arc<Mutex<Server>>,), Error = Infallible> + Clone {
    warp::any().map(move || server.clone())
}

fn json_body<'a, T: DeserializeOwned + Send>() -> impl Filter<Extract = (T,), Error = warp::Rejection> + Clone {
    warp::body::content_length_limit(1024 * 16)
        .and(warp::body::json())
}

fn body_bytes<'a>() -> impl Filter<Extract = (Bytes,), Error = warp::Rejection> + Clone {
    warp::body::bytes()
}

async fn websocket(uuid: String, wb: Ws, server_arc: Arc<Mutex<Server>>) -> Result<impl Reply, Rejection> {
    println!("STARTING WEBSOCKET!");
    let server_arc_clone = server_arc.clone();
    let server = server_arc_clone.lock().await;
    let accounts = server.accounts.lock().await.clone();
    let ids = server.ids.lock().await.clone();
    let (sender, mut receiver) = channel::<server::ServerMessage>(5);
    let mut event_stream_senders = server.event_stream_senders.lock().await;
    event_stream_senders.insert(uuid.clone(), sender);
    return Ok(wb.on_upgrade(move |mut websocket: WebSocket| async move {
        let email_message = websocket.next().await;
        let password_message = websocket.next().await;
        if password_message.is_some() && email_message.is_some() {
            if password_message.as_ref().unwrap().is_ok() && email_message.as_ref().unwrap().is_ok() {
                if password_message.as_ref().unwrap().as_ref().unwrap().to_str().is_ok() && email_message.as_ref().unwrap().as_ref().unwrap().to_str().is_ok() {
                    let password = password_message.unwrap().unwrap().to_str().unwrap().to_string();
                    let email = email_message.unwrap().unwrap().to_str().unwrap().to_string();
                    let mut hasher = DefaultHasher::new();
                    password.hash(&mut hasher);
                    let password_hash = hasher.finish();
                    println!("{} {} {}", accounts.contains_key(&email), accounts.get(&email).unwrap_or(&0) == &password_hash, ids.get(&uuid).unwrap_or(&IDSInfo { account: "".into(), uuid: "".into(), public_key: "".into() }).account == email);
                    if accounts.contains_key(&email) && accounts.get(&email).unwrap_or(&0) == &password_hash && ids.get(&uuid).unwrap_or(&IDSInfo { account: "".into(), uuid: "".into(), public_key: "".into() }).account == email {
                        println!("auth succeded");
                        let default = Vec::new();
                        let my_message_queue = server_arc.lock().await.message_queue.lock().await.get(&uuid).unwrap_or(&default).clone();
                        let mut failed = false;
                        for message_file in my_message_queue {
                            let message_bytes = fs::read(format!("messages/{message_file}"));
                            if message_bytes.is_err() {
                                eprintln!("{message_bytes:?}");
                                continue;
                            }
                            let _ = fs::remove_file(format!("messages/{message_file}"));
                            match websocket.start_send_unpin(ws::Message::binary(message_bytes.unwrap())) {
                                Ok(_) => {},
                                Err(e) => {println!("failed to send message{e}"); failed = true; break;}
                            };
                        }
                        if !failed {
                            server_arc.lock().await.message_queue.lock().await.remove(&uuid);
                            let mut interval = time::interval(Duration::from_secs_f32(0.5));
                            let mut seconds = 31.0;
                            loop {
                                seconds += 0.5;
                                if seconds >= 30.0 {
                                    match websocket.start_send_unpin(ws::Message::text(format!("😝"))) {
                                        Ok(_) => {},
                                        Err(e) => {println!("failed to send message{e}"); break;}
                                    };
                                    seconds = 0.0;
                                }
                                let message = receiver.try_recv();
                                if message.is_ok() {
                                    match websocket.start_send_unpin(ws::Message::binary(message.as_ref().unwrap().encode_to_vec())) {
                                        Ok(_) => {println!("sent message to device")},
                                        Err(e) => {println!("failed to send message{e}"); break;}
                                    };
                                }
                                interval.tick().await;
                            }
                        }
                    } else {
                        eprintln!("password hash did not match!");
                        let _ = websocket.start_send_unpin(ws::Message::text("password hash did not match!"));
                    }
                } else {
                    eprintln!("password is not a string!");
                    let _ = websocket.start_send_unpin(ws::Message::text("password is not a string!"));
                }
            } else {
                eprintln!("password is err!");
                let _ = websocket.start_send_unpin(ws::Message::text("password is err!"));
            }
        } else {
            eprintln!("no password sent!");
            let _ = websocket.start_send_unpin(ws::Message::text("no password sent!"));
        }
    }));
}

fn error_json(message: &str) -> String {
    return format!("{{\"success\": false, \"reason\": \"{message}\"}}");
}

async fn register_ids(device_info: IDSPost, server_arc: Arc<Mutex<Server>>) -> Result<impl Reply, Rejection> {
    let server = server_arc.lock().await;
    let accounts = server.accounts.lock().await;
    let mut hasher = DefaultHasher::new();
    device_info.password.hash(&mut hasher);
    let password_hash = hasher.finish();
    if accounts.contains_key(&device_info.email) && accounts.get(&device_info.email).unwrap_or(&0) == &password_hash {
        let uuid = uuid::Uuid::new_v4().to_string();
        let mut ids = server.ids.lock().await;
        ids.insert(uuid.clone(), IDSInfo { account: device_info.email, uuid: uuid.clone(), public_key: device_info.public_key });
        return Ok(format!("{{\"success\": true, \"uuid\": \"{uuid}\"}}"));
    }
    return Ok(error_json("auth failed"));
}

async fn create_account(account: Account, server_arc: Arc<Mutex<Server>>) -> Result<impl Reply, Rejection> {
    let server = server_arc.lock().await;
    if !server.accounts.lock().await.contains_key(&account.email) {
        let mut hasher = DefaultHasher::new();
        account.password.hash(&mut hasher);
        let password_hash = hasher.finish();
        server.accounts.lock().await.insert(account.email, password_hash);
        return Ok(format!("{{\"success\": true}}"));
    }
    Ok(error_json("account exists"))
}

async fn query_ids(account: String, server_arc: Arc<Mutex<Server>>) -> Result<impl Reply, Rejection> {
    let account = urlencoding::decode(&account).unwrap_or(std::borrow::Cow::Borrowed(""));
    let server = server_arc.lock().await;
    let ids = server.ids.lock().await;
    let account_ids = ids.iter().filter(|(_, device)| device.account == account ).map(|(_, device)| device).collect::<Vec<&IDSInfo>>();
    let ids_json = serde_json::to_string(&account_ids).expect("serialization failed");
    return Ok(format!("{{\"success\": true, \"ids\": {ids_json}}}"));
}

#[derive(Serialize)]
struct ServerMessage {
    data: String,
    sender: String,
}

async fn post_message(message_bytes: Bytes, server_arc: Arc<Mutex<Server>>) -> Result<impl Reply, Rejection> {
    let message = server::Message::decode(message_bytes);
    if message.is_err() {
        return Ok(error_json("data is not a valid message"));
    }
    let message = message.unwrap();
    let server = server_arc.lock().await;
    let accounts = server.accounts.lock().await;
    let mut hasher = DefaultHasher::new();
    message.password.hash(&mut hasher);
    let password_hash = hasher.finish();
    if accounts.contains_key(&message.email) && accounts.get(&message.email).unwrap_or(&0) == &password_hash {
        if server.ids.lock().await.contains_key(&message.recipient) {
            let mut sent = false;
            if server.event_stream_senders.lock().await.contains_key(&message.recipient) {
                match server.event_stream_senders.lock().await.get_mut(&message.recipient).unwrap().send(server::ServerMessage {data: message.data.clone(), sender: message.email.clone()}).await {
                    Ok(_) => {sent = true; println!("sent to socket successfully")},
                    Err(_) => {}
                }
            }
            if !sent {
                println!("adding message to queue");
                server.event_stream_senders.lock().await.remove(&message.recipient);
                if !server.message_queue.lock().await.contains_key(&message.recipient) {
                    server.message_queue.lock().await.insert(message.recipient.clone(), Vec::new());
                }
                let message_file = save_message(server::ServerMessage {data: message.data.clone(), sender: message.email});
                if message_file.is_ok() {
                    server.message_queue.lock().await.get_mut(&message.recipient).unwrap().push(message_file.unwrap());
                }
            }
            return Ok(format!("{{\"success\": true}}"));
        }
        return Ok(error_json("no recipient"));
    }
    return Ok(error_json("auth failed"));
}

fn save_message(message: server::ServerMessage) -> Result<String, std::io::Error> {
    let message_bytes = message.encode_to_vec();
    let file_name = uuid::Uuid::new_v4();
    fs::create_dir_all("messages/")?;
    let mut file = File::create(format!("messages/{file_name}"))?;
    file.write_all(&message_bytes)?;
    return Ok(file_name.to_string());
}