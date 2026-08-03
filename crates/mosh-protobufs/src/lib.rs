//! Wire encoding for transport instructions and state diffs.
//!
//! These messages are part of the wire contract: a C++ endpoint must be able to parse
//! what we write, and vice versa. The schemas here are byte-compatible with upstream's,
//! with proto2 extensions inlined as ordinary fields of the same number.

#![forbid(unsafe_code)]

pub mod transport {
    include!(concat!(env!("OUT_DIR"), "/transport_buffers.rs"));
}

pub mod host {
    include!(concat!(env!("OUT_DIR"), "/host_buffers.rs"));
}

pub mod user {
    include!(concat!(env!("OUT_DIR"), "/client_buffers.rs"));
}

pub use prost::Message;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_round_trips() {
        let inst = transport::Instruction {
            protocol_version: Some(2),
            old_num: Some(1),
            new_num: Some(2),
            ack_num: Some(3),
            throwaway_num: Some(0),
            diff: Some(b"payload".to_vec()),
            chaff: Some(vec![0; 4]),
        };
        let bytes = inst.encode_to_vec();
        assert_eq!(transport::Instruction::decode(&bytes[..]).unwrap(), inst);
    }

    #[test]
    fn absent_fields_stay_absent() {
        // proto2 semantics: an unset optional field must not be written at all, so a
        // peer can tell "not sent" from "sent as zero".
        let empty = transport::Instruction::default();
        assert!(empty.encode_to_vec().is_empty());

        let zeroed = transport::Instruction {
            protocol_version: Some(0),
            ..Default::default()
        };
        assert!(!zeroed.encode_to_vec().is_empty());
    }

    #[test]
    fn host_message_carries_several_instructions() {
        let msg = host::HostMessage {
            instruction: vec![
                host::Instruction {
                    hostbytes: Some(host::HostBytes {
                        hoststring: Some(b"abc".to_vec()),
                    }),
                    ..Default::default()
                },
                host::Instruction {
                    resize: Some(host::ResizeMessage {
                        width: Some(80),
                        height: Some(24),
                    }),
                    ..Default::default()
                },
                host::Instruction {
                    echoack: Some(host::EchoAck {
                        echo_ack_num: Some(99),
                    }),
                    ..Default::default()
                },
            ],
        };
        let bytes = msg.encode_to_vec();
        assert_eq!(host::HostMessage::decode(&bytes[..]).unwrap(), msg);
    }

    #[test]
    fn user_message_round_trips() {
        let msg = user::UserMessage {
            instruction: vec![user::Instruction {
                keystroke: Some(user::Keystroke {
                    keys: Some(b"hi".to_vec()),
                }),
                resize: None,
            }],
        };
        let bytes = msg.encode_to_vec();
        assert_eq!(user::UserMessage::decode(&bytes[..]).unwrap(), msg);
    }

    #[test]
    fn extension_fields_keep_their_upstream_numbers() {
        // hostbytes is extension field 2 upstream; inlining must not renumber it.
        let inst = host::Instruction {
            hostbytes: Some(host::HostBytes {
                hoststring: Some(b"x".to_vec()),
            }),
            ..Default::default()
        };
        let bytes = inst.encode_to_vec();
        // Tag byte = (field_number << 3) | wire_type; field 2, wire type 2 => 0x12.
        assert_eq!(bytes[0], 0x12, "hostbytes is no longer field 2");

        let resize = host::Instruction {
            resize: Some(host::ResizeMessage {
                width: Some(1),
                height: Some(1),
            }),
            ..Default::default()
        };
        // field 3, wire type 2 => 0x1a.
        assert_eq!(
            resize.encode_to_vec()[0],
            0x1a,
            "resize is no longer field 3"
        );
    }
}
