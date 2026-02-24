use serde::{Deserialize, Serialize};


#[derive(Serialize, Deserialize)]
pub enum Request {
    Run {
        name: String,
        command: Vec<String>
    },
    Attach {
        name: String
    },
    List,
    Delete {
        name: String,
        force: bool
    }
}

#[derive(Serialize, Deserialize)]
pub enum Response {
    RunSuccess,
    RunFailed(String),
    ForwardingReady,
    SessionNotExists,
    SessionList(Vec<String>),
    SessionDeletedSafely,
    SessionProcessNotExited,
    SessionDeletedForcely,
    SessionDropped
}