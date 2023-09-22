use std::{sync::Arc, collections::{HashMap, hash_map::DefaultHasher}, fs::{OpenOptions, File}, path::Path, io::Write, process::exit, convert::Infallible, net::SocketAddr, hash::{Hash, Hasher}, time::Duration};

use futures::{executor::block_on, StreamExt, SinkExt};
use serde::{Deserialize, de::DeserializeOwned, Serialize};
use tokio::{sync::{mpsc::{self, channel}, Mutex}, time};
use warp::{Filter, filters::ws::{Ws, WebSocket, self}, reply::Reply, reject::Rejection};

struct Server {
    event_stream_senders: Mutex<HashMap<String, mpsc::Sender<String>>>,
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
            println!("saved ids successfully")
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
        .and_then(websocket).or(
            warp::path!("register-ids")
            .and(json_body())
            .and(with_server(server_arc.clone()))
            .and_then(register_ids)
        ).or(
            warp::path!("create-account")
            .and(json_body())
            .and(with_server(server_arc.clone()))
            .and_then(create_account)
        ).or(
            warp::path!("query-ids" / String)
            .and(with_server(server_arc.clone()))
            .and_then(query_ids)
        ).or(
            warp::path!("post-message")
            .and(json_body())
            .and(with_server(server_arc.clone()))
            .and_then(post_message)
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

async fn websocket(uuid: String, wb: Ws, server_arc: Arc<Mutex<Server>>) -> Result<impl Reply, Rejection> {
    println!("STARTING WEBSOCKET!");
    let server_arc_clone = server_arc.clone();
    let server = server_arc_clone.lock().await;
    let accounts = server.accounts.lock().await.clone();
    let ids = server.ids.lock().await.clone();
    let (sender, mut receiver) = channel::<String>(5);
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
                    if accounts.contains_key(&email) && accounts.get(&email).unwrap_or(&0) == &password_hash && ids.get(&uuid).unwrap_or(&IDSInfo { account: "".into(), uuid: "".into(), public_key: "".into() }).account == email {
                        println!("auth succeded");
                        let server = server_arc.lock().await;
                        let mut message_queue = server.message_queue.lock().await;
                        let default = Vec::new();
                        let my_message_queue = message_queue.get(&uuid).unwrap_or(&default);
                        let mut failed = false;
                        for message in my_message_queue {
                            match websocket.start_send_unpin(ws::Message::text(message)) {
                                Ok(_) => {},
                                Err(e) => {println!("failed to send message{e}"); failed = true; break;}
                            };
                        }
                        if !failed {
                            message_queue.remove(&uuid);
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
                                    match websocket.start_send_unpin(ws::Message::text(format!("{}", message.as_ref().unwrap()))) {
                                        Ok(_) => {},
                                        Err(e) => {println!("failed to send message{e}"); break;}
                                    };
                                }
                                interval.tick().await;
                            }
                        }
                    } else {
                        eprintln!("password hash did not match!");
                    }
                } else {
                    eprintln!("password is not a string!");
                }
            } else {
                eprintln!("password is err!");
            }
        } else {
            eprintln!("no password sent!");
        }
        println!("auth failed!");
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

#[derive(Deserialize)]
struct Message {
    account: Account,
    recipient: String,
    data: String,
}

async fn post_message(message: Message, server_arc: Arc<Mutex<Server>>) -> Result<impl Reply, Rejection> {
    let server = server_arc.lock().await;
    let accounts = server.accounts.lock().await;
    let mut hasher = DefaultHasher::new();
    message.account.password.hash(&mut hasher);
    let password_hash = hasher.finish();
    if accounts.contains_key(&message.account.email) && accounts.get(&message.account.email).unwrap_or(&0) == &password_hash {
        if server.ids.lock().await.contains_key(&message.recipient) {
            let mut sent = false;
            if server.event_stream_senders.lock().await.contains_key(&message.recipient) {
                match server.event_stream_senders.lock().await.get_mut(&message.recipient).unwrap().send(message.data.clone()).await {
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
                server.message_queue.lock().await.get_mut(&message.recipient).unwrap().push(message.data.clone());
            }
            return Ok(format!("{{\"success\": true}}"));
        }
        return Ok(error_json("no recipient"));
    }
    return Ok(error_json("auth failed"));
}