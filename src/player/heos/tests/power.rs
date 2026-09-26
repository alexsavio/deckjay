//! The power key on a HEOS receiver: stop, then standby on the control port.

use std::io::Read;
use std::net::TcpListener;

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
fn a_speaker_without_a_control_port_still_stops() {
    let unused = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = unused.local_addr().unwrap().port();
    drop(unused);
    let mut rig = super::Rig::new(super::ONE_PLAYER);
    rig.player.control_port = closed;

    assert!(rig.player.standby().is_err());
    crate::player::power_off(&mut rig.player, &mut rig.emitter).unwrap();

    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
    assert_eq!(rig.events().last(), Some(&PlayerEvent::Stopped));
}

#[test]
fn the_power_key_stops_the_receiver_even_with_nothing_of_ours() {
    let mut rig = super::Rig::new(super::ONE_PLAYER);
    rig.player.stop_everything(&mut rig.emitter).unwrap();
    assert_eq!(
        rig.fake.last_command(),
        "player/set_play_state?pid=7&state=stop"
    );
}
