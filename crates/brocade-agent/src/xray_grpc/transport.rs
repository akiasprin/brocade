//! Retry only read-only RPCs after a blocking socket read timeout. A new attempt
//! owns a new connection and framing buffer; partial responses are never reused.

use super::*;
use std::{io, thread, time::Instant};

#[cfg(test)]
mod tests;

thread_local! {
    static SCOPED_DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Bound all RPCs in a runtime observation, including retries and multiple inbounds, so a
/// failed candidate cannot hold the accounting mutex away from the one-second guard.
pub(crate) struct DeadlineGuard(Option<Instant>);

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        SCOPED_DEADLINE.with(|deadline| deadline.set(self.0));
    }
}

pub(crate) fn deadline_after(timeout: Option<Duration>) -> DeadlineGuard {
    let previous = SCOPED_DEADLINE.with(|deadline| {
        let previous = deadline.get();
        if let Some(timeout) = timeout {
            let next = Instant::now() + timeout;
            deadline.set(Some(previous.map_or(next, |previous| previous.min(next))));
        }
        previous
    });
    DeadlineGuard(previous)
}

#[derive(Clone, Copy)]
struct Policy {
    io_timeout: Duration,
    read_budget: Duration,
    retry_delay: Duration,
    max_attempts: u32,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            io_timeout: Duration::from_secs(5),
            read_budget: Duration::from_secs(10),
            retry_delay: Duration::from_millis(100),
            max_attempts: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Connect,
    Write,
    Read,
}

enum Failure {
    Io(Stage, io::Error),
    Protocol(String),
}

impl Failure {
    fn retryable_read(&self) -> bool {
        matches!(self, Self::Io(Stage::Read, error) if matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut))
    }

    fn summary(&self) -> String {
        match self {
            Self::Io(stage, error) => format!(
                "stage={stage:?} kind={:?} errno={:?}",
                error.kind(),
                error.raw_os_error()
            ),
            Self::Protocol(_) => "stage=Protocol".to_owned(),
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Io(Stage::Connect, error) => format!("connect to xray gRPC api: {error}"),
            Self::Io(Stage::Write, error) => format!("write xray gRPC request: {error}"),
            Self::Io(Stage::Read, error) if self.retryable_read() => {
                format!("[XRAY_API_READ_TIMEOUT] read xray gRPC response: {error}")
            }
            Self::Io(Stage::Read, error) => format!("read xray gRPC response: {error}"),
            Self::Protocol(error) => error.clone(),
        }
    }
}

fn read_only(path: &str, request: &[u8]) -> bool {
    // QueryStats/GetUsersStats can reset counters. Fail closed for malformed
    // request fields, without rejecting or changing the single-attempt call.
    let reset_field = match path {
        QUERY_STATS_PATH | GET_USERS_STATS_PATH => Some(2),
        _ => None,
    };
    if let Some(reset_field) = reset_field {
        let mut cursor = 0;
        loop {
            match next_field(request, &mut cursor) {
                Ok(Some((number, Field::Varint(0)))) if number == reset_field => {}
                Ok(Some((number, _))) if number == reset_field || number == 0 => return false,
                Ok(Some(_)) => {}
                Ok(None) => return true,
                Err(_) => return false,
            }
        }
    }
    // Unknown new methods are non-retryable until explicitly audited here.
    matches!(
        path,
        LIST_INBOUNDS_PATH
            | FORWARDING_POLICY_PATH
            | GET_INBOUND_USERS_PATH
            | MUX_SNAPSHOT_PATH
            | REVERSE_HEALTH_SNAPSHOT_PATH
    )
}

pub(super) fn unary(
    api_address: SocketAddr,
    path: &str,
    request: &[u8],
    outer_deadline: Option<Instant>,
) -> Result<Vec<u8>, String> {
    let outer_deadline = match (
        outer_deadline,
        SCOPED_DEADLINE.with(|deadline| deadline.get()),
    ) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    };
    unary_with_policy(
        api_address,
        path,
        request,
        outer_deadline,
        Policy::default(),
    )
}

fn unary_with_policy(
    api_address: SocketAddr,
    path: &str,
    request: &[u8],
    outer_deadline: Option<Instant>,
    policy: Policy,
) -> Result<Vec<u8>, String> {
    let started = Instant::now();
    let can_retry = read_only(path, request);
    let budget = if can_retry {
        policy.read_budget
    } else {
        policy.io_timeout * 3
    };
    let deadline = outer_deadline.map_or(started + budget, |outer| outer.min(started + budget));
    let attempts = if can_retry { policy.max_attempts } else { 1 };
    let method = path.rsplit('/').next().unwrap_or("unknown");
    let mut first = None;
    for attempt in 1..=attempts {
        match unary_once(api_address, path, request, deadline, policy.io_timeout) {
            Ok(response) => {
                if let Some(first) = first {
                    eprintln!("[XRAY_API_RETRY_RECOVERED] method={method} api={api_address} attempts={attempt} elapsed_ms={} first={first}", started.elapsed().as_millis());
                }
                return Ok(response);
            }
            Err(error) => {
                let first = first.get_or_insert_with(|| error.summary());
                let remaining = deadline.saturating_duration_since(Instant::now());
                let delay = policy.retry_delay.saturating_mul(attempt);
                if !can_retry
                    || !error.retryable_read()
                    || attempt == attempts
                    || remaining <= delay
                {
                    let unconfirmed = if !can_retry
                        && matches!(error, Failure::Io(Stage::Read | Stage::Write, _))
                    {
                        "；结果未确认，不直接重放变更请求"
                    } else {
                        ""
                    };
                    return Err(format!("{}{unconfirmed}；method={method} api={api_address} attempts={attempt} elapsed_ms={} first={first} last={}", error.message(), started.elapsed().as_millis(), error.summary()));
                }
                thread::sleep(delay);
            }
        }
    }
    unreachable!("the private retry policy always permits at least one attempt")
}

struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
    io_timeout: Duration,
}

impl DeadlineStream {
    fn timeout(&self) -> io::Result<Duration> {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Xray API operation deadline exceeded",
            ))
        } else {
            Ok(self.io_timeout.min(remaining))
        }
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.timeout()?))?;
        self.stream.read(bytes)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.timeout()?))?;
        self.stream.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

fn unary_once(
    api_address: SocketAddr,
    path: &str,
    request: &[u8],
    deadline: Instant,
    io_timeout: Duration,
) -> Result<Vec<u8>, Failure> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(Failure::Io(
            Stage::Read,
            io::Error::new(
                io::ErrorKind::TimedOut,
                "Xray API operation deadline exceeded",
            ),
        ));
    }
    let stream = TcpStream::connect_timeout(&api_address, io_timeout.min(remaining))
        .map_err(|error| Failure::Io(Stage::Connect, error))?;
    let _ = stream.set_nodelay(true);
    let mut stream = DeadlineStream {
        stream,
        deadline,
        io_timeout,
    };
    let mut message = Vec::with_capacity(request.len() + 5);
    message.push(0);
    message.extend_from_slice(
        &u32::try_from(request.len())
            .map_err(|_| Failure::Protocol("xray gRPC request is too large".to_owned()))?
            .to_be_bytes(),
    );
    message.extend_from_slice(request);

    stream
        .write_all(CLIENT_PREFACE)
        .and_then(|_| write_frame(&mut stream, 4, 0, 0, &settings_payload()))
        // SETTINGS changes only stream windows; the connection window starts at
        // 64 KiB and must be opened separately for large snapshots.
        .and_then(|_| {
            write_frame(
                &mut stream,
                8,
                0,
                0,
                &(WINDOW_SIZE - DEFAULT_WINDOW_SIZE).to_be_bytes(),
            )
        })
        .and_then(|_| {
            write_frame(
                &mut stream,
                1,
                END_HEADERS,
                1,
                &request_headers(api_address, path),
            )
        })
        .and_then(|_| write_frame(&mut stream, 0, END_STREAM, 1, &message))
        .and_then(|_| stream.flush())
        .map_err(|error| Failure::Io(Stage::Write, error))?;

    let mut response_data = Vec::new();
    loop {
        let frame = read_frame(&mut stream).map_err(|error| Failure::Io(Stage::Read, error))?;
        match frame.kind {
            0 if frame.stream_id == 1 => {
                if response_data.len().saturating_add(frame.payload.len()) > WINDOW_SIZE as usize {
                    return Err(Failure::Protocol(
                        "xray gRPC response exceeds snapshot limit".to_owned(),
                    ));
                }
                response_data.extend_from_slice(&frame.payload);
            }
            3 if frame.stream_id == 1 => {
                return Err(Failure::Protocol("xray gRPC reset the stream".to_owned()))
            }
            4 if frame.flags & ACK == 0 => write_frame(&mut stream, 4, ACK, 0, &[])
                .map_err(|error| Failure::Io(Stage::Write, error))?,
            6 if frame.flags & ACK == 0 && frame.payload.len() == 8 => {
                write_frame(&mut stream, 6, ACK, 0, &frame.payload)
                    .map_err(|error| Failure::Io(Stage::Write, error))?
            }
            7 => {
                return Err(Failure::Protocol(
                    "xray gRPC closed the connection".to_owned(),
                ))
            }
            _ => {}
        }
        if frame.stream_id == 1 && frame.flags & END_STREAM != 0 {
            return grpc_response_message(&response_data).map_err(Failure::Protocol);
        }
    }
}
