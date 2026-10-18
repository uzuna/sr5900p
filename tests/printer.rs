//! Printer against an in-process fake that speaks the UDP control protocol
//! and accepts the TCP raster stream on the same port.
//! The fake replaces the device only: the request sequence, the UDP response timeout,
//! the print timeout and the clean-up of `Printer` run as in production.
//! The TCP write timeout is exercised only with the slowly reading fake; the TCP connect
//! timeout is not (localhost connects at once). The fake does not model device timing or how
//! it reacts to the TCP connection closing.
use sr5900p::label::display::TapeDisplay;
use sr5900p::label::Label;
use sr5900p::printer::gen_tcp_data;
use sr5900p::printer::Printer;
use sr5900p::printer::PrinterError;
use sr5900p::printer::Status;
use sr5900p::Tape;
use std::io::Read;
use std::net::TcpListener;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const TAPE_W12: u8 = 0x03;

#[derive(Clone)]
struct Behavior {
    /// byte 0x02 of the status body while idle (0x06 = no tape)
    idle_state: u8,
    /// number of status polls answered with "printing" after the stream arrived;
    /// `None` keeps answering "printing" forever
    printing_polls: Option<usize>,
    /// byte 0x02 of the status body once printing is over (0x42 = tape ran out)
    done_state: u8,
    /// body of the reply to the second data stream request (0x10 = accepted)
    confirm_reply: Vec<u8>,
    /// leave start print unanswered
    silent_start: bool,
    /// leave status unanswered once the stream has arrived
    silent_polls: bool,
    /// read the TCP stream slowly and never stop on the end marker
    slow_reader: bool,
    /// leave stop print unanswered
    silent_stop: bool,
    /// answer nothing at all
    silent: bool,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            idle_state: 0,
            printing_polls: Some(2),
            done_state: 0,
            confirm_reply: vec![0x10],
            silent_start: false,
            silent_polls: false,
            slow_reader: false,
            silent_stop: false,
            silent: false,
        }
    }
}

#[derive(Default)]
struct Log {
    commands: Vec<u32>,
    stream: Vec<u8>,
}

struct Fake {
    port: u16,
    log: Arc<Mutex<Log>>,
}

impl Fake {
    fn start(behavior: Behavior) -> Self {
        // the printer uses one port number for UDP and TCP, so take a free TCP port and bind UDP
        // to it; the UDP side may already be taken by a client socket of a test running in
        // parallel, so try another port then
        let (listener, port, udp) = (0..100)
            .find_map(|_| {
                let listener = TcpListener::bind("127.0.0.1:0").unwrap();
                let port = listener.local_addr().unwrap().port();
                match UdpSocket::bind(("127.0.0.1", port)) {
                    Ok(udp) => Some((listener, port, udp)),
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => None,
                    Err(e) => panic!("{e}"),
                }
            })
            .expect("no port free for both TCP and UDP");
        let log = Arc::new(Mutex::new(Log::default()));
        let streamed = Arc::new(Mutex::new(false));

        let tcp_log = log.clone();
        let tcp_streamed = streamed.clone();
        let slow_reader = behavior.slow_reader;
        thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                if slow_reader {
                    let mut chunk = [0u8; 4096];
                    while let Ok(n) = conn.read(&mut chunk) {
                        if n == 0 {
                            break;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    return;
                }
                let mut buf = Vec::new();
                // the client never closes the stream, so read until the end marker arrives
                let mut chunk = [0u8; 4096];
                while let Ok(n) = conn.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if buf.ends_with(&[0x0c, 27, 123, 3, 64, 64, 125]) {
                        break;
                    }
                }
                tcp_log.lock().unwrap().stream = buf;
                *tcp_streamed.lock().unwrap() = true;
            }
        });

        let udp_log = log.clone();
        thread::spawn(move || {
            let mut polls_left = behavior.printing_polls;
            let mut buf = [0u8; 128];
            while let Ok((len, from)) = udp.recv_from(&mut buf) {
                assert_eq!(len, 32);
                assert_eq!(&buf[..4], b"TPRT");
                let cmd = u32::from_be_bytes(buf[16..20].try_into().unwrap());
                udp_log.lock().unwrap().commands.push(cmd);
                if behavior.silent
                    || (cmd == 1 && behavior.silent_polls && *streamed.lock().unwrap())
                    || (cmd == 2 && behavior.silent_start)
                    || (cmd == 3 && behavior.silent_stop)
                {
                    continue;
                }
                let body: Vec<u8> = match cmd {
                    1 => {
                        let printing = *streamed.lock().unwrap()
                            && match polls_left.as_mut() {
                                None => true,
                                Some(0) => false,
                                Some(n) => {
                                    *n -= 1;
                                    true
                                }
                            };
                        let state = if *streamed.lock().unwrap() {
                            behavior.done_state
                        } else {
                            behavior.idle_state
                        };
                        let mut d = vec![20, 0, state, TAPE_W12];
                        d.extend_from_slice(&[0, 0, 0, 0, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
                        if printing {
                            d[1] = 2;
                        }
                        d
                    }
                    2 => vec![2, 0, 0],
                    3 => vec![3, 0, 0],
                    0x0101 => vec![0],
                    0x0100 => behavior.confirm_reply.clone(),
                    _ => vec![],
                };
                let mut res = b"tprt".to_vec();
                for v in [0u32, 1, 0x20, cmd, body.len() as u32, 0, 0] {
                    res.extend_from_slice(&v.to_be_bytes());
                }
                res.extend_from_slice(&body);
                udp.send_to(&res, from).unwrap();
            }
        });
        Fake { port, log }
    }

    fn printer(&self) -> Printer {
        Printer::new("127.0.0.1".parse().unwrap())
            .port(self.port)
            .timeout(Duration::from_millis(300))
    }

    fn commands(&self) -> Vec<u32> {
        self.log.lock().unwrap().commands.clone()
    }
}

fn label(tape: Tape) -> Label {
    let height = tape.width_px() as usize;
    let mut td = TapeDisplay::new(30, height);
    td.set_pixel(0, 0, true);
    td.set_pixel(29, height - 1, true);
    Label::new(td, tape).unwrap()
}

#[test]
fn status_reports_loaded_tape() {
    let fake = Fake::start(Behavior::default());
    assert_eq!(
        fake.printer().status().unwrap(),
        Status::Idle { tape: Tape::W12 }
    );
}

#[test]
fn status_times_out_on_a_silent_printer() {
    // Why: without a read timeout the call blocked forever when the printer was off
    let fake = Fake::start(Behavior {
        silent: true,
        ..Default::default()
    });
    // run on another thread so that a missing timeout fails the test instead of hanging it
    let printer = fake.printer();
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || tx.send(printer.status()).unwrap());
    let result = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("status() did not return");
    let err = result.unwrap_err();
    assert!(
        matches!(err, PrinterError::Timeout { step: "status", .. }),
        "{err:?}"
    );
}

#[test]
fn print_sends_the_stream_and_stops() {
    let fake = Fake::start(Behavior::default());
    let td = label(Tape::W12);
    fake.printer().print(&td).unwrap();
    let cmds = fake.commands();
    // status, start, data stream prepare/ready, status polls (2 printing + 1 done), stop
    assert_eq!(cmds, vec![1, 2, 0x0101, 0x0100, 1, 1, 1, 3]);
    assert_eq!(fake.log.lock().unwrap().stream, gen_tcp_data(td.bitmap()));
}

#[test]
fn print_rejects_a_label_for_another_tape_before_starting() {
    let fake = Fake::start(Behavior::default());
    let td = label(Tape::W18);
    let err = fake.printer().print(&td).unwrap_err();
    assert!(
        matches!(
            err,
            PrinterError::TapeMismatch {
                label: Tape::W18,
                loaded: Tape::W12
            }
        ),
        "{err:?}"
    );
    assert_eq!(fake.commands(), vec![1]);
}

#[test]
fn print_refuses_when_no_tape_is_loaded() {
    let fake = Fake::start(Behavior {
        idle_state: 0x06,
        ..Default::default()
    });
    let err = fake.printer().print(&label(Tape::W12)).unwrap_err();
    assert!(matches!(err, PrinterError::NoTape), "{err:?}");
    assert_eq!(fake.commands(), vec![1]);
}

/// Run on another thread so that a missing limit fails the test instead of hanging it.
fn print_within(printer: Printer, label: Label, limit: Duration) -> Result<(), PrinterError> {
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || tx.send(printer.print(&label)).unwrap());
    rx.recv_timeout(limit).expect("print() did not return")
}

#[test]
fn print_gives_up_after_print_timeout_and_still_stops() {
    let fake = Fake::start(Behavior {
        printing_polls: None,
        ..Default::default()
    });
    let printer = fake.printer().print_timeout(Duration::from_secs(1));
    let err = print_within(printer, label(Tape::W12), Duration::from_secs(6)).unwrap_err();
    assert!(matches!(err, PrinterError::PrintTimeout(_)), "{err:?}");
    assert_eq!(fake.commands().last(), Some(&3));
}

#[test]
fn print_reports_a_status_other_than_idle_after_printing() {
    // e.g. the tape ran out in the middle of the label
    let fake = Fake::start(Behavior {
        done_state: 0x42,
        ..Default::default()
    });
    let err = fake.printer().print(&label(Tape::W12)).unwrap_err();
    assert!(
        matches!(err, PrinterError::UnexpectedStatus(Status::Unknown(d)) if d[2] == 0x42),
        "{err:?}"
    );
    assert_eq!(fake.commands().last(), Some(&3));
}

#[test]
fn print_fails_when_stop_is_not_acknowledged() {
    let fake = Fake::start(Behavior {
        silent_stop: true,
        ..Default::default()
    });
    let err = fake.printer().print(&label(Tape::W12)).unwrap_err();
    assert!(
        matches!(
            err,
            PrinterError::Timeout {
                step: "stop print",
                ..
            }
        ),
        "{err:?}"
    );
}

#[test]
fn print_returns_the_send_error_rather_than_the_stop_error() {
    let fake = Fake::start(Behavior {
        confirm_reply: vec![0x99],
        silent_stop: true,
        ..Default::default()
    });
    let err = fake.printer().print(&label(Tape::W12)).unwrap_err();
    assert!(
        matches!(&err, PrinterError::Protocol(m) if m.starts_with("confirm data stream")),
        "{err:?}"
    );
    assert_eq!(fake.commands(), vec![1, 2, 0x0101, 0x0100, 3]);
}

#[test]
fn print_sends_stop_when_the_start_reply_is_lost() {
    // the start request may have reached the printer even though its reply did not come back
    let fake = Fake::start(Behavior {
        silent_start: true,
        ..Default::default()
    });
    let err = fake.printer().print(&label(Tape::W12)).unwrap_err();
    assert!(
        matches!(
            err,
            PrinterError::Timeout {
                step: "start print",
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(fake.commands(), vec![1, 2, 3]);
}

#[test]
fn print_timeout_also_bounds_an_unanswered_poll() {
    // a poll waits for at most what is left of print_timeout, not the full response timeout
    let fake = Fake::start(Behavior {
        silent_polls: true,
        ..Default::default()
    });
    let printer = fake
        .printer()
        .timeout(Duration::from_secs(3))
        .print_timeout(Duration::from_millis(800));
    let started = std::time::Instant::now();
    let err = print_within(printer, label(Tape::W12), Duration::from_secs(10)).unwrap_err();
    assert!(matches!(err, PrinterError::PrintTimeout(_)), "{err:?}");
    // 1.5 s of pauses between the steps + 0.8 s of waiting; a full 3 s poll would exceed this
    assert!(
        started.elapsed() < Duration::from_millis(3500),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(fake.commands().last(), Some(&3));
}

#[test]
fn timeout_bounds_the_whole_raster_send() {
    // the fake keeps reading slowly, so no single write ever times out
    let fake = Fake::start(Behavior {
        slow_reader: true,
        ..Default::default()
    });
    let printer = fake.printer().timeout(Duration::from_millis(500));
    let data = vec![0u8; 64 << 20];
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || tx.send(printer.print_raw(&data)).unwrap());
    let err = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("print_raw() did not return")
        .unwrap_err();
    assert!(
        matches!(
            err,
            PrinterError::Timeout {
                step: "send raster",
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(fake.commands().last(), Some(&3));
}
