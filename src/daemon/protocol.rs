use std::io::{self, Read, Write};
use std::path::PathBuf;

use crate::terminal_pane::PaneId;

use super::{AiHookState, AiTool, DaemonEvent, ReplayedPane};

/// クライアント → デーモン メッセージ
#[derive(Debug)]
pub(super) enum ClientMsg {
    Hello,
    Spawn {
        pane_id: PaneId,
        cols: u16,
        rows: u16,
        cwd: String,
    },
    Input {
        pane_id: PaneId,
        data: Vec<u8>,
    },
    Resize {
        pane_id: PaneId,
        cols: u16,
        rows: u16,
    },
    Kill {
        pane_id: PaneId,
    },
    QueryCwds,
    Clear {
        pane_id: PaneId,
    },
    HookEvent {
        pane_id: PaneId,
        state: AiHookState,
        tool: Option<AiTool>,
    },
}

const TAG_HELLO: u8 = 0x01;
const TAG_SPAWN: u8 = 0x02;
const TAG_INPUT: u8 = 0x03;
const TAG_RESIZE: u8 = 0x04;
const TAG_KILL: u8 = 0x05;
const TAG_QUERY_CWDS: u8 = 0x06;
const TAG_CLEAR: u8 = 0x07;
const TAG_HOOK_EVENT: u8 = 0x08;

const TAG_REPLAY: u8 = 0x81;
const TAG_HELLO_DONE: u8 = 0x82;
const TAG_OUTPUT: u8 = 0x83;
const TAG_EXITED: u8 = 0x84;
const TAG_CWD_LIST: u8 = 0x85;
const TAG_RESYNC: u8 = 0x86;
const TAG_HOOK_EVENT_EV: u8 = 0x87;
const TAG_AI_TOOL_LIST: u8 = 0x88;

// =========================================================================
// バイナリフレーム読み書き（外部シリアライズクレート不要）
// =========================================================================

fn write_frame(w: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let mut header = [0u8; 5];
    header[0] = tag;
    header[1..5].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&header)?;
    if !payload.is_empty() {
        w.write_all(payload)?;
    }
    w.flush()
}

fn read_frame(r: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0u8; 5];
    r.read_exact(&mut header)?;
    let tag = header[0];
    let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
    // 単一フレーム最大 16MB ガード
    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut payload)?;
    }
    Ok((tag, payload))
}

pub(super) fn write_client_msg(w: &mut impl Write, msg: &ClientMsg) -> io::Result<()> {
    match msg {
        ClientMsg::Hello => write_frame(w, TAG_HELLO, &[]),
        ClientMsg::Spawn {
            pane_id,
            cols,
            rows,
            cwd,
        } => {
            let cwd_b = cwd.as_bytes();
            let mut p = Vec::with_capacity(8 + 2 + 2 + cwd_b.len());
            p.extend_from_slice(&pane_id.to_le_bytes());
            p.extend_from_slice(&cols.to_le_bytes());
            p.extend_from_slice(&rows.to_le_bytes());
            p.extend_from_slice(cwd_b);
            write_frame(w, TAG_SPAWN, &p)
        }
        ClientMsg::Input { pane_id, data } => {
            let mut p = Vec::with_capacity(8 + data.len());
            p.extend_from_slice(&pane_id.to_le_bytes());
            p.extend_from_slice(data);
            write_frame(w, TAG_INPUT, &p)
        }
        ClientMsg::Resize {
            pane_id,
            cols,
            rows,
        } => {
            let mut p = Vec::with_capacity(12);
            p.extend_from_slice(&pane_id.to_le_bytes());
            p.extend_from_slice(&cols.to_le_bytes());
            p.extend_from_slice(&rows.to_le_bytes());
            write_frame(w, TAG_RESIZE, &p)
        }
        ClientMsg::Kill { pane_id } => write_frame(w, TAG_KILL, &pane_id.to_le_bytes()),
        ClientMsg::QueryCwds => write_frame(w, TAG_QUERY_CWDS, &[]),
        ClientMsg::Clear { pane_id } => write_frame(w, TAG_CLEAR, &pane_id.to_le_bytes()),
        ClientMsg::HookEvent {
            pane_id,
            state,
            tool,
        } => {
            let mut p = [0u8; 10];
            p[0..8].copy_from_slice(&pane_id.to_le_bytes());
            p[8] = *state as u8;
            p[9] = tool.map_or(0, |t| t as u8);
            write_frame(w, TAG_HOOK_EVENT, &p)
        }
    }
}

pub(super) fn read_client_msg(r: &mut impl Read) -> io::Result<ClientMsg> {
    let (tag, p) = read_frame(r)?;
    match tag {
        TAG_HELLO => Ok(ClientMsg::Hello),
        TAG_SPAWN if p.len() >= 12 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            let cols = u16::from_le_bytes(p[8..10].try_into().unwrap());
            let rows = u16::from_le_bytes(p[10..12].try_into().unwrap());
            let cwd = String::from_utf8_lossy(&p[12..]).into_owned();
            Ok(ClientMsg::Spawn {
                pane_id,
                cols,
                rows,
                cwd,
            })
        }
        TAG_INPUT if p.len() >= 8 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            Ok(ClientMsg::Input {
                pane_id,
                data: p[8..].to_vec(),
            })
        }
        TAG_RESIZE if p.len() >= 12 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            let cols = u16::from_le_bytes(p[8..10].try_into().unwrap());
            let rows = u16::from_le_bytes(p[10..12].try_into().unwrap());
            Ok(ClientMsg::Resize {
                pane_id,
                cols,
                rows,
            })
        }
        TAG_KILL if p.len() >= 8 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            Ok(ClientMsg::Kill { pane_id })
        }
        TAG_QUERY_CWDS => Ok(ClientMsg::QueryCwds),
        TAG_CLEAR if p.len() >= 8 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            Ok(ClientMsg::Clear { pane_id })
        }
        TAG_HOOK_EVENT if p.len() >= 10 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            let state = AiHookState::from_u8(p[8])
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid hook state"))?;
            let tool = AiTool::from_u8(p[9]);
            Ok(ClientMsg::HookEvent {
                pane_id,
                state,
                tool,
            })
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown client message",
        )),
    }
}

fn write_replayed_pane_frame(w: &mut impl Write, tag: u8, rp: &ReplayedPane) -> io::Result<()> {
    let cwd_b = rp.cwd.to_string_lossy();
    let cwd_bytes = cwd_b.as_bytes();
    let mut p = Vec::with_capacity(8 + 2 + 2 + 4 + cwd_bytes.len() + rp.replay_bytes.len());
    p.extend_from_slice(&rp.pane_id.to_le_bytes());
    p.extend_from_slice(&rp.cols.to_le_bytes());
    p.extend_from_slice(&rp.rows.to_le_bytes());
    p.extend_from_slice(&(cwd_bytes.len() as u32).to_le_bytes());
    p.extend_from_slice(cwd_bytes);
    p.extend_from_slice(&rp.replay_bytes);
    write_frame(w, tag, &p)
}

fn parse_replayed_pane_payload(p: &[u8]) -> io::Result<ReplayedPane> {
    let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
    let cols = u16::from_le_bytes(p[8..10].try_into().unwrap());
    let rows = u16::from_le_bytes(p[10..12].try_into().unwrap());
    let cwd_len = u32::from_le_bytes(p[12..16].try_into().unwrap()) as usize;
    if p.len() < 16 + cwd_len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "truncated replay",
        ));
    }
    let cwd = PathBuf::from(String::from_utf8_lossy(&p[16..16 + cwd_len]).into_owned());
    let replay_bytes = p[16 + cwd_len..].to_vec();
    Ok(ReplayedPane {
        pane_id,
        cols,
        rows,
        cwd,
        replay_bytes,
    })
}

pub(super) fn write_daemon_event(w: &mut impl Write, ev: &DaemonEvent) -> io::Result<()> {
    match ev {
        DaemonEvent::Replay(rp) => write_replayed_pane_frame(w, TAG_REPLAY, rp),
        DaemonEvent::Resync(rp) => write_replayed_pane_frame(w, TAG_RESYNC, rp),
        DaemonEvent::HelloDone {
            next_pane_id,
            protocol_version,
        } => {
            let mut p = Vec::with_capacity(12);
            p.extend_from_slice(&next_pane_id.to_le_bytes());
            p.extend_from_slice(&protocol_version.to_le_bytes());
            write_frame(w, TAG_HELLO_DONE, &p)
        }
        DaemonEvent::Output { pane_id, data } => {
            let mut p = Vec::with_capacity(8 + data.len());
            p.extend_from_slice(&pane_id.to_le_bytes());
            p.extend_from_slice(data);
            write_frame(w, TAG_OUTPUT, &p)
        }
        DaemonEvent::Exited { pane_id } => write_frame(w, TAG_EXITED, &pane_id.to_le_bytes()),
        DaemonEvent::CwdList(list) => {
            let mut p = Vec::new();
            p.extend_from_slice(&(list.len() as u32).to_le_bytes());
            for (pid, cwd) in list {
                let s = cwd.to_string_lossy();
                let b = s.as_bytes();
                p.extend_from_slice(&pid.to_le_bytes());
                p.extend_from_slice(&(b.len() as u32).to_le_bytes());
                p.extend_from_slice(b);
            }
            write_frame(w, TAG_CWD_LIST, &p)
        }
        DaemonEvent::HookEvent {
            pane_id,
            state,
            tool,
        } => {
            let mut p = [0u8; 10];
            p[0..8].copy_from_slice(&pane_id.to_le_bytes());
            p[8] = *state as u8;
            p[9] = tool.map_or(0, |t| t as u8);
            write_frame(w, TAG_HOOK_EVENT_EV, &p)
        }
        DaemonEvent::AiToolList(list) => {
            let mut p = Vec::with_capacity(4 + list.len() * 9);
            p.extend_from_slice(&(list.len() as u32).to_le_bytes());
            for (pid, tool) in list {
                p.extend_from_slice(&pid.to_le_bytes());
                p.push(tool.map_or(0, |t| t as u8));
            }
            write_frame(w, TAG_AI_TOOL_LIST, &p)
        }
    }
}

pub(super) fn read_daemon_event(r: &mut impl Read) -> io::Result<DaemonEvent> {
    let (tag, p) = read_frame(r)?;
    match tag {
        TAG_REPLAY if p.len() >= 16 => Ok(DaemonEvent::Replay(parse_replayed_pane_payload(&p)?)),
        TAG_RESYNC if p.len() >= 16 => Ok(DaemonEvent::Resync(parse_replayed_pane_payload(&p)?)),
        TAG_HELLO_DONE if p.len() >= 8 => {
            let next_pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            let protocol_version = if p.len() >= 12 {
                u32::from_le_bytes(p[8..12].try_into().unwrap())
            } else {
                // 旧プロトコル（バージョンフィールド導入前）
                1
            };
            Ok(DaemonEvent::HelloDone {
                next_pane_id,
                protocol_version,
            })
        }
        TAG_OUTPUT if p.len() >= 8 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            Ok(DaemonEvent::Output {
                pane_id,
                data: p[8..].to_vec(),
            })
        }
        TAG_EXITED if p.len() >= 8 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            Ok(DaemonEvent::Exited { pane_id })
        }
        TAG_CWD_LIST if p.len() >= 4 => {
            let count = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
            let mut off = 4;
            let mut list = Vec::with_capacity(count);
            for _ in 0..count {
                if p.len() < off + 12 {
                    break;
                }
                let pane_id = u64::from_le_bytes(p[off..off + 8].try_into().unwrap());
                let len = u32::from_le_bytes(p[off + 8..off + 12].try_into().unwrap()) as usize;
                off += 12;
                if p.len() < off + len {
                    break;
                }
                let cwd = PathBuf::from(String::from_utf8_lossy(&p[off..off + len]).into_owned());
                off += len;
                list.push((pane_id, cwd));
            }
            Ok(DaemonEvent::CwdList(list))
        }
        TAG_HOOK_EVENT_EV if p.len() >= 10 => {
            let pane_id = u64::from_le_bytes(p[0..8].try_into().unwrap());
            let state = AiHookState::from_u8(p[8])
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid hook state"))?;
            let tool = AiTool::from_u8(p[9]);
            Ok(DaemonEvent::HookEvent {
                pane_id,
                state,
                tool,
            })
        }
        TAG_AI_TOOL_LIST if p.len() >= 4 => {
            let count = u32::from_le_bytes(p[0..4].try_into().unwrap()) as usize;
            let mut off = 4;
            let mut list = Vec::with_capacity(count);
            for _ in 0..count {
                if p.len() < off + 9 {
                    break;
                }
                let pane_id = u64::from_le_bytes(p[off..off + 8].try_into().unwrap());
                let tool = AiTool::from_u8(p[off + 8]);
                off += 9;
                list.push((pane_id, tool));
            }
            Ok(DaemonEvent::AiToolList(list))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unknown daemon event",
        )),
    }
}
