use edgli::{
    EdgliProcesses,
    hopr_lib::{Address, SessionClientConfig, SessionTarget},
    run_hopr_edge_node,
};

#[derive(Debug, thiserror::Error)]
pub enum HoprError {
    #[error("Construction error: {0}")]
    Construction(String),

    #[error("HoprLib error: {0}")]
    HoprLib(#[from] edgli::hopr_lib::errors::HoprLibError),

    // --- session errors ---
    #[error("Session error: {0}")]
    Session(String),

    #[error("Listen host already used")]
    ListenHostAlreadyUsed,

    #[error("Session not found")]
    SessionNotFound,
}

pub struct Hopr {
    hopr: edgli::hopr_lib::Hopr,
    rt: tokio::runtime::Runtime,
    processes: Vec<EdgliProcesses>,
}

impl Hopr {
    pub fn new(
        cfg: edgli::hopr_lib::config::HoprLibConfig,
        keys: edgli::hopr_lib::HoprKeys,
    ) -> std::result::Result<Self, HoprError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| HoprError::Construction(e.to_string()))?;

        let (hopr, processes) = rt
            .block_on(run_hopr_edge_node(cfg, keys))
            .map_err(|e| HoprError::Construction(e.to_string()))?;

        Ok(Self { hopr, rt, processes })
    }

    // --- session management ---

    /// Open a local port and return the configuration
    pub fn open_session(
        &self,
        destination: Address,
        target: SessionTarget,
        cfg: SessionClientConfig,
    ) -> std::result::Result<(), HoprError> {
        let session = self.rt.block_on(self.hopr.connect_to(destination, target, cfg))?;

        // open a port and bind the session execution to it, return the port

        Ok(())
    }

    pub fn close_session(&self, session_id: u64) -> std::result::Result<(), HoprError> {
        // self.rt.block_on(self.hopr.close_session(session_id))?;

        Ok(())
    }

    pub fn list_sessions(
        &self,
        protocol: crate::session::Protocol,
    ) -> std::result::Result<Vec<SessionClientConfig>, HoprError> {
        // let sessions = self.rt.block_on(self.hopr.list_sessions(protocol))?;
        // Ok(sessions)

        unimplemented!()
    }

    pub fn update_session(
        &self,
        max_surb_upstream: String,
        session_buffer: String,
        client: String,
    ) -> std::result::Result<Vec<SessionClientConfig>, HoprError> {
        // let sessions = self.rt.block_on(self.hopr.list_sessions(protocol))?;
        // Ok(sessions)

        unimplemented!()
    }
}

impl Drop for Hopr {
    fn drop(&mut self) {
        for process in &mut self.processes {
            match process {
                EdgliProcesses::HoprLib(_process, handle) => handle.abort(),
                EdgliProcesses::Hopr(handle) => handle.abort(),
            }
        }
    }
}
