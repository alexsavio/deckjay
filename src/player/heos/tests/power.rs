//! The power key on a HEOS receiver: stop, then standby on the control port.

use std::io::Read;
use std::net::TcpListener;
use std::sync::mpsc;

use super::*;

#[test]
fn standby_sends_the_receiver_its_standby_command() {
    let control = TcpListener::bind("127.0.0.1:0").unwrap();
    let mut player = HeosPlayer::new("127.0.0.1".into(), 1);
    player.control_port = control.local_addr().unwrap().port();

    player.standby().unwrap();

    let (mut stream, _) = control.accept().unwrap();
    let mut sent = String::new();
    stream.read_to_string(&mut sent).unwrap();
    assert_eq!(sent, "PWSTANDBY\r");
}

#[test]
fn a_speaker_without_a_control_port_fails_standby_but_stops() {
    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = unused.local_addr().unwrap().port();
    drop(unused);
    let mut player = HeosPlayer::new("127.0.0.1".into(), port);
    player.control_port = port;
    let (tx, events) = mpsc::channel();
    let mut emitter = Emitter::new(tx);

    assert!(player.standby().is_err());
    crate::player::power_off(&mut player, &mut emitter).unwrap();

    assert_eq!(events.try_iter().last(), Some(PlayerEvent::Stopped));
}
