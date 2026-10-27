//! adb failures to `TransportError`, as `translateTransportError` in the TS
//! engine (`transport/adb-server.ts` in the ReTV app) does, except that
//! text it does not recognise is `Internal` (the wire's `internal`).

use std::io;

use retv_bridge_core::api::TransportError;

/// Maps a failure message from the adb server (a `FAIL` payload or the text
/// of a `host:connect` reply) to its transport code. Anything unrecognised is
/// `Internal`: the caller logs the text, and the wire only says `internal`.
pub fn map_adb_failure(msg: &str) -> TransportError {
    let lower = msg.to_ascii_lowercase();
    if retv_bridge_core::api::is_no_space(msg) {
        TransportError::NoSpace
    } else if lower.contains("unauthorized") {
        TransportError::Unauthorized
    } else if lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == "offline")
    {
        TransportError::DeviceOffline
    } else if lower.contains("failed to connect")
        || lower.contains("cannot connect")
        || lower.contains("connection refused")
        || lower.contains("no route")
    {
        TransportError::DeviceUnreachable
    } else if lower.contains("not found") {
        TransportError::DeviceNotFound
    } else {
        TransportError::Internal
    }
}

/// Opening the socket to the adb server failed: the server is not running.
pub(crate) fn server_connect_error(_err: io::Error) -> TransportError {
    TransportError::AdbServerUnreachable
}

/// A read or write on an open server socket failed (EOF, reset): the
/// connection dropped mid-request.
pub(crate) fn stream_error(_err: io::Error) -> TransportError {
    TransportError::ConnectionLost
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_tv_is_no_space() {
        assert_eq!(
            map_adb_failure("remote couldn't create file: No space left on device"),
            TransportError::NoSpace
        );
    }
}
