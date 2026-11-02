use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::daemon::{self, Capture};
use crate::discovery::ScannerInfo;
use crate::error::{Error, Result};
use crate::escl::{self, Format, ScanSettings, SettingsError};
use crate::http;
use crate::identity::Identity;
use crate::scan::Sheet;
use mdns_sd::{ServiceDaemon, ServiceInfo};

const JOB_WAIT: Duration = Duration::from_secs(60);
const KEPT_JOBS: usize = 10;

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

struct Job {
    id: String,
    format: Format,
    created: Instant,
    data: Mutex<JobData>,
    ready: Condvar,
    cancel: AtomicBool,
}

struct JobData {
    state: &'static str,
    reason: &'static str,
    images: u32,
    delivered: u32,
    docs: VecDeque<Vec<u8>>,
    finished: bool,
}

impl Job {
    fn new(format: Format) -> Self {
        Job {
            id: new_id(),
            format,
            created: Instant::now(),
            data: Mutex::new(JobData {
                state: "Processing",
                reason: "JobScanning",
                images: 0,
                delivered: 0,
                docs: VecDeque::new(),
                finished: false,
            }),
            ready: Condvar::new(),
            cancel: AtomicBool::new(false),
        }
    }

    fn finished(&self) -> bool {
        self.data.lock().unwrap().finished
    }

    fn content_type(&self) -> &'static str {
        match self.format {
            Format::Jpeg => "image/jpeg",
            Format::Pdf => "application/pdf",
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
    adf: Arc<Mutex<AdfState>>,
    jobs: Mutex<Vec<Arc<Job>>>,
    mdns: Mutex<Option<ServiceDaemon>>,
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
            adf: Arc::new(Mutex::new(AdfState::Loaded)),
            jobs: Mutex::new(Vec::new()),
            mdns: Mutex::new(None),
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
        if self.info.lock().unwrap().is_some() {
            self.advertise();
        }
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
        escl::status_xml(state, adf, &self.job_views())
    }

    fn job_views(&self) -> Vec<escl::JobView> {
        let jobs = self.jobs.lock().unwrap();
        jobs.iter()
            .rev()
            .map(|job| {
                let data = job.data.lock().unwrap();
                escl::JobView {
                    id: job.id.clone(),
                    age_secs: job.created.elapsed().as_secs(),
                    images: data.images,
                    to_transfer: data.images.saturating_sub(data.delivered),
                    state: data.state,
                    reason: data.reason,
                }
            })
            .collect()
    }

    fn advertise(&self) {
        let mut slot = self.mdns.lock().unwrap();
        if slot.is_some() {
            return;
        }
        let Some(info) = self.info.lock().unwrap().clone() else {
            return;
        };
        let instance = escl::instance_name(&info.model, &info.serial);
        let uuid = escl::device_uuid(&info.model, &info.serial);
        let ip = crate::iface::local_ip_towards(self.cfg.ip).unwrap_or(Ipv4Addr::LOCALHOST);
        let properties: HashMap<String, String> =
            txt_records(&instance, &uuid).into_iter().collect();
        let registered = (|| -> std::result::Result<ServiceDaemon, Box<dyn std::error::Error>> {
            let daemon = ServiceDaemon::new()?;
            let service = ServiceInfo::new(
                "_uscan._tcp.local.",
                &instance,
                &host_name(&info.serial),
                ip.to_string(),
                self.local.port(),
                properties,
            )?;
            daemon.register(service)?;
            Ok(daemon)
        })();
        match registered {
            Ok(daemon) => *slot = Some(daemon),
            Err(err) => eprintln!("freejitsu: bonjour: {err}"),
        }
    }

    fn capabilities(&self) -> Option<String> {
        let info = self.info.lock().unwrap().clone()?;
        Some(escl::capabilities_xml(&escl::Capabilities {
            model: &info.model,
            serial: &info.serial,
            uuid: &escl::device_uuid(&info.model, &info.serial),
        }))
    }

    fn find_job(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .find(|job| job.id == id)
            .cloned()
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
            ("POST", "/eSCL/ScanJobs") => self.create_job(request),
            ("GET", path) => match document_id(path) {
                Some(id) => self.next_document(id),
                None => http::Response::empty(404),
            },
            ("DELETE", path) => match job_id(path) {
                Some(id) => self.delete_job(id),
                None => http::Response::empty(404),
            },
            _ => http::Response::empty(404),
        }
    }

    fn create_job(&self, request: &http::Request) -> http::Response {
        let settings = match escl::parse_scan_settings(&request.body) {
            Ok(settings) => settings,
            Err(SettingsError::BadXml(_)) => return http::Response::empty(400),
            Err(SettingsError::Unsupported(_)) => return http::Response::empty(409),
        };
        let Some(session) = self.session.lock().unwrap().clone() else {
            return http::Response::empty(503);
        };
        let Some(guard) = self.lock.try_acquire() else {
            return http::Response::empty(503);
        };

        let job = Arc::new(Job::new(settings.format));
        {
            let mut jobs = self.jobs.lock().unwrap();
            prune(&mut jobs);
            jobs.push(Arc::clone(&job));
        }

        let location = format!(
            "http://{}/eSCL/ScanJobs/{}",
            request.header("host").unwrap_or("127.0.0.1"),
            job.id
        );
        let worker = Worker {
            cfg: self.cfg.clone(),
            session,
            settings,
            job: Arc::clone(&job),
            adf: Arc::clone(&self.adf),
            guard,
        };
        thread::spawn(move || worker.run());

        let mut data = job.data.lock().unwrap();
        let deadline = Instant::now() + JOB_WAIT;
        while data.images == 0 && !data.finished {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let (next, _) = job.ready.wait_timeout(data, deadline - now).unwrap();
            data = next;
        }
        let failed = (data.images == 0 && data.finished).then_some(data.reason);
        drop(data);

        match failed {
            Some("AdfEmpty") => http::Response::empty(409),
            Some(_) => http::Response::empty(500),
            None => http::Response::empty(201).header("Location", location),
        }
    }

    fn next_document(&self, id: &str) -> http::Response {
        let Some(job) = self.find_job(id) else {
            return http::Response::empty(404);
        };
        let mut data = job.data.lock().unwrap();
        loop {
            if let Some(document) = data.docs.pop_front() {
                data.delivered += 1;
                return http::Response::bytes(200, job.content_type(), document);
            }
            if data.finished {
                return http::Response::empty(404);
            }
            data = job.ready.wait(data).unwrap();
        }
    }

    fn delete_job(&self, id: &str) -> http::Response {
        let Some(job) = self.find_job(id) else {
            return http::Response::empty(404);
        };
        job.cancel.store(true, Ordering::Release);
        http::Response::empty(200)
    }

    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_nonblocking(false);
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
        if let Some(daemon) = self.mdns.lock().unwrap().take() {
            let _ = daemon.shutdown();
        }
    }
}

struct Worker {
    cfg: Config,
    session: Session,
    settings: ScanSettings,
    job: Arc<Job>,
    adf: Arc<Mutex<AdfState>>,
    guard: ScanGuard,
}

impl Worker {
    fn run(self) {
        let Worker {
            cfg,
            session,
            settings,
            job,
            adf,
            guard,
        } = self;

        let result = daemon::capture(
            &cfg,
            &session.info,
            &session.identity,
            settings.dpi,
            settings.area,
            !settings.duplex,
            &mut |sheet: &Sheet| {
                if job.cancel.load(Ordering::Acquire) {
                    return Err(Error::Canceled);
                }
                let mut data = job.data.lock().unwrap();
                data.images += if settings.duplex { 2 } else { 1 };
                if settings.format == Format::Jpeg {
                    let sides = if settings.duplex {
                        vec![&sheet.front, &sheet.back]
                    } else {
                        vec![&sheet.front]
                    };
                    for jpeg in sides {
                        data.docs.push_back(escl::with_orientation_180(jpeg)?);
                    }
                }
                drop(data);
                job.ready.notify_all();
                Ok(())
            },
        );

        let mut read_error = None;
        if settings.format == Format::Pdf
            && let Ok(capture) = &result
        {
            match std::fs::read(&capture.path) {
                Ok(document) => job.data.lock().unwrap().docs.push_back(document),
                Err(err) => read_error = Some(Error::Io(err)),
            }
        }

        let (state, reason) = match &result {
            Ok(capture) => match read_error.as_ref().or(capture.early.as_ref()) {
                Some(err) => aborted(err),
                None => ("Completed", "JobCompletedSuccessfully"),
            },
            Err(err) => aborted(err),
        };

        {
            let mut data = job.data.lock().unwrap();
            data.state = state;
            data.reason = reason;
            data.finished = true;
        }
        job.ready.notify_all();
        note_result(&adf, &result);
        drop(guard);
    }
}

fn aborted(err: &Error) -> (&'static str, &'static str) {
    match err {
        Error::Canceled => ("Canceled", "JobCanceledByUser"),
        Error::NoPaper => ("Aborted", "AdfEmpty"),
        Error::Feed(_) => ("Aborted", "MediaJam"),
        _ => ("Aborted", "ErrorOccurred"),
    }
}

fn prune(jobs: &mut Vec<Arc<Job>>) {
    while jobs.iter().filter(|job| job.finished()).count() >= KEPT_JOBS {
        let Some(index) = jobs.iter().position(|job| job.finished()) else {
            return;
        };
        jobs.remove(index);
    }
}

fn new_id() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS entropy for a job id");
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn job_id(path: &str) -> Option<&str> {
    path.strip_prefix("/eSCL/ScanJobs/")
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

fn document_id(path: &str) -> Option<&str> {
    path.strip_prefix("/eSCL/ScanJobs/")?
        .strip_suffix("/NextDocument")
        .filter(|id| !id.is_empty() && !id.contains('/'))
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

pub fn txt_records(instance: &str, uuid: &str) -> Vec<(String, String)> {
    [
        ("txtvers", "1"),
        ("vers", "2.63"),
        ("rs", "eSCL"),
        ("ty", instance),
        ("pdl", "image/jpeg,application/pdf"),
        ("uuid", uuid),
        ("cs", "color"),
        ("is", "adf"),
        ("duplex", "T"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect()
}

pub fn host_name(serial: &str) -> String {
    let label: String = serial
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("freejitsu-{label}.local.")
}
