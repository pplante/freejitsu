use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::config::Config;
use crate::daemon::Capture;
use crate::discovery::ScannerInfo;
use crate::error::{Error, Result};
use crate::escl;
use crate::http;
use crate::identity::Identity;

#[derive(Default)]
pub struct ScanLock {
    busy: AtomicBool,
}

impl ScanLock {
    pub fn new() -> Self {
        ScanLock::default()
    }

    pub fn try_acquire(self: &Arc<Self>) -> Option<ScanGuard> {
        self.busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| ScanGuard {
                lock: Arc::clone(self),
            })
    }

    pub fn busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }
}

pub struct ScanGuard {
    lock: Arc<ScanLock>,
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.lock.busy.store(false, Ordering::Release);
    }
}

#[derive(Clone)]
pub struct Session {
    pub info: ScannerInfo,
    pub identity: Identity,
}

#[derive(Clone, Copy)]
enum AdfState {
    Loaded,
    Empty,
}

impl AdfState {
    fn name(self) -> &'static str {
        match self {
            AdfState::Loaded => "ScannerAdfLoaded",
            AdfState::Empty => "ScannerAdfEmpty",
        }
    }
}

pub struct AirScan {
    cfg: Config,
    lock: Arc<ScanLock>,
    listener: TcpListener,
    local: SocketAddr,
    stop: AtomicBool,
    accept: Mutex<Option<JoinHandle<()>>>,
    session: Mutex<Option<Session>>,
    info: Mutex<Option<ScannerInfo>>,
    adf: Mutex<AdfState>,
}

impl AirScan {
    pub fn start(cfg: Config, lock: Arc<ScanLock>) -> Result<Arc<Self>> {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, cfg.airscan_port))?;
        listener.set_nonblocking(true)?;
        let local = listener.local_addr()?;
        let service = Arc::new(AirScan {
            cfg,
            lock,
            listener,
            local,
            stop: AtomicBool::new(false),
            accept: Mutex::new(None),
            session: Mutex::new(None),
            info: Mutex::new(None),
            adf: Mutex::new(AdfState::Loaded),
        });
        let weak = Arc::downgrade(&service);
        let thread = thread::spawn(move || accept_loop(weak));
        *service.accept.lock().unwrap() = Some(thread);
        Ok(service)
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub fn set_session(&self, session: Option<Session>) {
        if let Some(session) = &session {
            *self.info.lock().unwrap() = Some(session.info.clone());
        }
        *self.session.lock().unwrap() = session;
    }

    pub fn note_result(&self, result: &Result<Capture>) {
        note_result(&self.adf, result);
    }

    pub fn status(&self) -> String {
        let state = if self.lock.busy() {
            "Processing"
        } else if self.session.lock().unwrap().is_none() {
            "Stopped"
        } else {
            "Idle"
        };
        let adf = self.adf.lock().unwrap().name();
        escl::status_xml(state, adf, &[])
    }

    fn capabilities(&self) -> Option<String> {
        let info = self.info.lock().unwrap().clone()?;
        Some(escl::capabilities_xml(&escl::Capabilities {
            model: &info.model,
            serial: &info.serial,
            uuid: &escl::device_uuid(&info.model, &info.serial),
        }))
    }

    fn route(&self, request: &http::Request) -> http::Response {
        match (request.method.as_str(), request.path.as_str()) {
            ("GET" | "HEAD", "/eSCL/ScannerCapabilities") => match self.capabilities() {
                Some(xml) => http::Response::xml(xml),
                None => http::Response::empty(503),
            },
            ("GET" | "HEAD", "/eSCL/ScannerStatus") => {
                if self.info.lock().unwrap().is_none() {
                    http::Response::empty(503)
                } else {
                    http::Response::xml(self.status())
                }
            }
            _ => http::Response::empty(404),
        }
    }

    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(30)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(120)));
        let request = match http::read_request(&mut stream) {
            Ok(request) => request,
            Err(http::RequestError::Eof | http::RequestError::Io(_)) => return,
            Err(http::RequestError::Malformed(_)) => {
                let _ = http::write_response(&mut stream, false, &http::Response::empty(400));
                return;
            }
            Err(http::RequestError::TooLarge) => {
                let _ = http::write_response(&mut stream, false, &http::Response::empty(413));
                return;
            }
        };
        let head_only = request.method == "HEAD";
        let response = self.route(&request);
        let _ = http::write_response(&mut stream, head_only, &response);
    }
}

impl Drop for AirScan {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.local);
        if let Some(thread) = self.accept.lock().unwrap().take() {
            let _ = thread.join();
        }
    }
}

fn accept_loop(weak: Weak<AirScan>) {
    loop {
        let Some(service) = weak.upgrade() else {
            return;
        };
        let stopping = service.stop.load(Ordering::Acquire);
        drop(service);
        if stopping {
            return;
        }
        match weak.upgrade() {
            Some(service) => match service.listener.accept() {
                Ok((stream, _)) => {
                    drop(service);
                    let weak = Weak::clone(&weak);
                    thread::spawn(move || {
                        if let Some(service) = weak.upgrade() {
                            service.handle(stream);
                        }
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    drop(service);
                    thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            },
            None => return,
        }
    }
}

fn note_result(adf: &Mutex<AdfState>, result: &Result<Capture>) {
    let state = match result {
        Ok(_) => AdfState::Loaded,
        Err(Error::NoPaper | Error::Feed(_)) => AdfState::Empty,
        Err(_) => return,
    };
    *adf.lock().unwrap() = state;
}
