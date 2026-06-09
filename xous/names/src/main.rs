use std::collections::{HashMap, HashSet};

use app_manifest::Manifest;
use log::{error, info};
use num_traits::FromPrimitive;
use system_manifests::SYSTEM_MANIFESTS;
use xous::{AppId, MessageEnvelope, MessageId};
use xous_api_names::*;

const PROCESS_DISCONNECTED_MESSAGE_ID: MessageId = 8;

#[derive(PartialEq)]
#[repr(C)]
enum ConnectSuccess {
    /// The server connection was successfully made
    Connected(xous::CID /* Connection ID */),

    /// There is no server with that name -- block this message
    Wait,
}

#[derive(Default)]
struct NameServer {
    waiting_connections: Vec<MessageEnvelope>,
    name_table: HashMap<String, xous::SID>,
    registered_names_by_pid: HashMap<xous::PID, HashSet<String>>,
    message_name_to_id: HashMap<(String, String), Vec<MessageId>>,
    server_owners: HashMap<String, AppId>,
    register_permissions: HashMap<AppId, HashSet<String>>,
    connect_permissions: HashMap<AppId, HashMap<String, Vec<MessageId>>>,
}

fn manifest_server_names(manifest: &Manifest) -> HashSet<String> {
    manifest.servers.keys().chain(manifest.fixed_sids.keys()).cloned().collect()
}

impl NameServer {
    fn process_manifest_servers(&mut self, manifest: &Manifest) -> Result<(), xous::Error> {
        let app_id = AppId(manifest.app_id);
        let server_names = manifest_server_names(manifest);

        self.reject_server_name_collisions(app_id, &server_names, &manifest.app_name_en())?;

        let previously_owned_names: HashSet<String> = self
            .server_owners
            .iter()
            .filter_map(|(server_name, owner)| (*owner == app_id).then(|| server_name.clone()))
            .collect();
        let replace_message_names: HashSet<String> =
            previously_owned_names.union(&server_names).cloned().collect();

        self.message_name_to_id.retain(|(server_name, _), _| !replace_message_names.contains(server_name));

        for server_name in previously_owned_names.difference(&server_names) {
            self.server_owners.remove(server_name);
        }
        for server_name in &server_names {
            self.server_owners.insert(server_name.clone(), app_id);
        }

        let mut app_register_permissions = HashSet::new();
        for (server_name, messages) in &manifest.servers {
            app_register_permissions.insert(server_name.clone());
            for (message_name, message) in messages {
                self.message_name_to_id.insert((server_name.clone(), message_name.clone()), vec![message.id]);
            }
        }
        for (server_name, sid_str) in &manifest.fixed_sids {
            if let Some(sid) = xous::SID::from_bytes(sid_str.as_bytes()) {
                self.name_table.insert(server_name.clone(), sid);
            } else {
                log::error!("Invalid SID string: {sid_str}");
            }
        }
        self.register_permissions.insert(app_id, app_register_permissions);
        Ok(())
    }

    fn reject_server_name_collisions(
        &self,
        app_id: AppId,
        server_names: &HashSet<String>,
        app_name: &str,
    ) -> Result<(), xous::Error> {
        for server_name in server_names {
            let owner = self.server_owners.get(server_name).copied();
            if let Some(owner_app_id) = owner {
                if owner_app_id != app_id {
                    log::error!(
                        "Rejected manifest for `{app_name}`: server `{server_name}` is already owned by app_id=0x{}",
                        hex::encode(owner_app_id.0)
                    );
                    return Err(xous::Error::MemoryInUse);
                }
            }

            if self.name_table.contains_key(server_name) && owner != Some(app_id) {
                log::error!(
                    "Rejected manifest for `{app_name}`: server `{server_name}` is already registered"
                );
                return Err(xous::Error::MemoryInUse);
            }
        }

        Ok(())
    }

    fn process_manifest_permissions(&mut self, manifest: &Manifest) -> Result<(), xous::Error> {
        let app_id = AppId(manifest.app_id);
        let mut app_permissions: HashMap<String, Vec<MessageId>> = HashMap::new();
        for (server_name, messages) in manifest.permissions.iter() {
            let server_permissions = app_permissions.entry(server_name.clone()).or_default();
            for message_name in messages {
                let Some(message_id) =
                    self.message_name_to_id.get(&(server_name.clone(), message_name.clone()))
                else {
                    log::error!("Rejected manifest for `{}`, couldn't find `{message_name}` message for `{server_name}` server", manifest.app_name_en());
                    return Err(xous::Error::ServerNotFound);
                };

                server_permissions.extend_from_slice(message_id);
            }
            server_permissions.sort_unstable();
        }
        // Insert only after the whole manifest validated, so a rejected
        // manifest leaves the app's existing permissions untouched (atomic
        // replace rather than a partial in-place mutation).
        self.connect_permissions.insert(app_id, app_permissions);
        Ok(())
    }

    fn process_manifest_update(&mut self, manifest: &Manifest) -> Result<(), xous::Error> {
        let mut staged = NameServer {
            waiting_connections: Vec::new(),
            name_table: self.name_table.clone(),
            registered_names_by_pid: self.registered_names_by_pid.clone(),
            message_name_to_id: self.message_name_to_id.clone(),
            server_owners: self.server_owners.clone(),
            register_permissions: self.register_permissions.clone(),
            connect_permissions: self.connect_permissions.clone(),
        };

        staged.process_manifest_servers(manifest)?;
        staged.process_manifest_permissions(manifest)?;

        self.name_table = staged.name_table;
        self.message_name_to_id = staged.message_name_to_id;
        self.server_owners = staged.server_owners;
        self.register_permissions = staged.register_permissions;
        self.connect_permissions = staged.connect_permissions;
        Ok(())
    }

    /// Connect to the server named in the message. If the server exists, attempt the connection
    /// and return either the connection ID or an error.
    ///
    /// If the server does not exist, return `Ok(None)`
    fn connect_impl(&self, msg: &MessageEnvelope) -> Result<ConnectSuccess, xous::Error> {
        let app_id = xous::get_app_id(msg.sender.pid().ok_or(xous::Error::ProcessNotFound)?)?
            .ok_or(xous::Error::ProcessNotFound)?;
        let server_name = name_from_msg(msg, 0)?;
        let sender_pid = msg.sender.pid().expect("kernel provided us a PID of None");
        log::trace!("BlockingConnect request for '{}' for process {:?}", server_name, sender_pid);

        let default_permissions = HashMap::default();
        let app_permissions = self.connect_permissions.get(&app_id).unwrap_or(&default_permissions);
        // If the server already exists, attempt to make the connection. The connection can
        // only succeed if the server is in the name_table.
        if let Some(server_sid) = self.name_table.get(server_name) {
            log::trace!(
                "Found entry in the table (sid: {:?}) -- attempting to call connect_for_process()",
                server_sid,
            );
            let no_perms = Vec::new();
            let Some(connection_permissions) = app_permissions
                .get(server_name)
                .or_else(|| self.register_permissions.get(&app_id)?.get(server_name).map(|_| &no_perms))
            else {
                log::warn!("App {app_id:02x?} tried to connect to {server_name} without permissions.");
                return Err(xous::Error::AccessDenied);
            };
            let result = xous::connect_for_process(sender_pid, *server_sid);
            match result {
                Ok(connection_id) => {
                    for permission in connection_permissions {
                        xous::allow_messages_on_connection(
                            sender_pid,
                            connection_id,
                            *permission..*permission + 1,
                        )
                        .expect("permissions");
                    }

                    log::trace!(
                        "Connected to '{}' for process {:?} with CID {}",
                        server_name,
                        sender_pid,
                        connection_id,
                    );
                    return Ok(ConnectSuccess::Connected(connection_id));
                }
                Err(e) => {
                    log::error!("error when making connection from pid {sender_pid} to {server_name}, perhaps the server crashed? {e:?}");
                    return Err(e);
                }
            }
        }

        // There is no connection, so block the sender
        log::trace!("No server currently registered to '{}', blocking...", server_name);
        Ok(ConnectSuccess::Wait)
    }

    fn register_impl(&mut self, msg: &MessageEnvelope) -> Result<String, xous::Error> {
        let sid = sid_from_msg(msg)?;
        let server_name = name_from_msg(msg, 16)?.to_string();
        let pid = msg.sender.pid().ok_or(xous::Error::ProcessNotFound)?;
        let app_id = xous::get_app_id(pid)?.ok_or(xous::Error::ProcessNotFound)?;
        log::trace!("registration request for '{}'", server_name);

        // Borrow register_permissions only long enough to produce a `bool`; the
        // shared borrow ends before we mutate name_table below.
        if !self.register_permissions.get(&app_id).ok_or(xous::Error::AccessDenied)?.contains(&server_name) {
            return Err(xous::Error::AccessDenied);
        }
        if self.name_table.contains_key(&server_name) {
            return Err(xous::Error::MemoryInUse);
        }
        self.name_table.insert(server_name.clone(), sid);
        self.registered_names_by_pid.entry(pid).or_default().insert(server_name.clone());
        log::trace!("request successful, SID is {:?}", sid);
        Ok(server_name)
    }

    fn unregister_pid(&mut self, pid: xous::PID) {
        if let Some(server_names) = self.registered_names_by_pid.remove(&pid) {
            for server_name in server_names {
                self.name_table.remove(&server_name);
                info!("unregistered '{}' because process {} disconnected", server_name, pid);
            }
        }
    }

    fn wake_waiting_connections(&mut self, server_name: &str) {
        // See if we have any requests matching this server ID. If so, make the
        // connection and don't retain it.
        let mut i = 0;
        while i < self.waiting_connections.len() {
            if name_from_msg(&self.waiting_connections[i], 0) == Ok(server_name) {
                let msg = self.waiting_connections.swap_remove(i);
                match self.connect_impl(&msg) {
                    Err(e) => respond_error(msg, e),
                    Ok(ConnectSuccess::Connected(cid)) => respond_connect_success(msg, cid),
                    Ok(ConnectSuccess::Wait) => {
                        panic!("message connection attempt resulted in `Wait` even though it ought to exist");
                    }
                }
            } else {
                i += 1;
            }
        }
    }

    fn process_system_manifests(&mut self) {
        let parsed_manifests: Vec<Manifest> = SYSTEM_MANIFESTS
            .iter()
            .map(|m| app_manifest::try_from_bytes(m.as_bytes()).expect("Could not parse system manifest"))
            .collect();
        // Two stage processing, so that permissions can refer to message IDs declared in later manifests.
        for manifest in &parsed_manifests {
            if self.process_manifest_servers(&manifest).is_err() {
                // Xtask should have prevented this from even building, so it's panic-worthy.
                panic!(
                    "Could not parse servers or groups in manifest for appID=0x{}.",
                    hex::encode(manifest.app_id)
                );
            };
        }
        for manifest in parsed_manifests {
            if self.process_manifest_permissions(&manifest).is_err() {
                // Xtask should have prevented this from even building, so it's panic-worthy.
                panic!(
                    "Could not parse permissions in manifest for appID=0x{}.",
                    hex::encode(manifest.app_id)
                );
            }
        }
    }

    fn add_manifest_impl(&mut self, msg: &MessageEnvelope) -> Result<(), xous::Error> {
        let app_id = xous::get_app_id(msg.sender.pid().ok_or(xous::Error::ProcessNotFound)?)?
            .ok_or(xous::Error::ProcessNotFound)?;
        if !self
            .connect_permissions
            .get(&app_id)
            .ok_or(xous::Error::AccessDenied)?
            .get("os/nameserver")
            .ok_or(xous::Error::AccessDenied)?
            .contains(&(api::Opcode::AddManifest as usize))
        {
            return Err(xous::Error::AccessDenied);
        }
        let msg = msg.body.memory_message().ok_or(xous::Error::InvalidArguments)?;
        let buf = msg.buf.as_slice::<u8>();
        // `valid` is required for this opcode: the caller must set it to the exact manifest
        // length. Defaulting to buf.len() would silently parse zero-padded trailing data and
        // make malformed requests harder to detect.
        let valid_bytes = msg
            .valid
            .ok_or_else(|| {
                log::error!("add_manifest_impl: msg.valid is None; manifest length is required");
                xous::Error::InvalidArguments
            })?
            .get();
        // `valid` is sender-controlled; reject oversized values before slicing.
        if valid_bytes > buf.len() {
            log::error!(
                "add_manifest_impl: valid_bytes ({valid_bytes}) exceeds buffer length ({})",
                buf.len()
            );
            return Err(xous::Error::InvalidArguments);
        }
        log::trace!("Parsing manifest, len={valid_bytes}");

        let manifest = app_manifest::try_from_bytes(&buf[..valid_bytes]).map_err(|e| {
            log::error!("Error parsing manifest: {e:?}");
            xous::Error::ParseError
        })?;

        log::trace!("Parsed manifest for `{}`", manifest.app_name_en());

        self.process_manifest_update(&manifest)?;
        Ok(())
    }

    fn run(&mut self) -> ! {
        // Init logging (may fail before logging server has started)
        log_server::init_wait(env!("CARGO_CRATE_NAME")).ok();
        log::set_max_level(log::LevelFilter::Info);
        xous::set_thread_priority(xous::ThreadPriority::Highest).unwrap();
        // TODO: Only allow privileged clients to add new permissions (SFT-5024, SFT-5025)
        let name_server =
            xous::create_server_with_sid(xous::SID::from_bytes(b"xous-name-server").unwrap(), 0..8)
                .expect("Couldn't create xous-name-server");
        xous::register_server_event_handler(
            xous::ServerEvent::Disconnected,
            name_server,
            PROCESS_DISCONNECTED_MESSAGE_ID,
        )
        .expect("Couldn't register xous-name-server disconnect handler");

        info!("Starting");
        self.process_system_manifests();

        loop {
            let msg = xous::receive_message(name_server).unwrap();
            log::trace!("received message: {:?}", msg);
            if msg.body.id() == PROCESS_DISCONNECTED_MESSAGE_ID {
                if let Some(pid) = msg.sender.pid() {
                    self.unregister_pid(pid);
                } else {
                    error!("disconnect event did not include a sender PID");
                }
                continue;
            }
            if !msg.body.is_blocking() {
                continue;
            }
            if !msg.body.has_memory() {
                xous::return_scalar(msg.sender, 0).unwrap();
                continue;
            }
            match FromPrimitive::from_usize(msg.body.id()) {
                Some(api::Opcode::Register) => match self.register_impl(&msg) {
                    Ok(name) => {
                        respond_simple_success(msg);
                        self.wake_waiting_connections(&name);
                    }
                    Err(err) => respond_error(msg, err),
                },
                Some(api::Opcode::BlockingConnect) | Some(api::Opcode::TryConnect) => {
                    match self.connect_impl(&msg) {
                        Err(e) => respond_error(msg, e),
                        Ok(ConnectSuccess::Connected(cid)) => respond_connect_success(msg, cid),
                        Ok(ConnectSuccess::Wait) => {
                            if msg.body.id() == api::Opcode::TryConnect as usize {
                                respond_error(msg, xous::Error::ServerNotFound);
                            } else {
                                // Push waiting connections here, which will prevent it from getting
                                // dropped and responded to.
                                self.waiting_connections.push(msg);
                            }
                        }
                    }
                }
                Some(api::Opcode::AddManifest) => match self.add_manifest_impl(&msg) {
                    Ok(()) => respond_simple_success(msg),
                    Err(err) => respond_error(msg, err),
                },
                None => {
                    error!("couldn't decode message: {:?}", msg);
                }
            }
        }
    }
}

fn sid_from_msg(env: &MessageEnvelope) -> Result<xous::SID, xous::Error> {
    let msg = env.body.memory_message().ok_or(xous::Error::InvalidArguments)?;
    // A Register message must carry at least 16 bytes (four u32 SID components).
    let u32_slice = msg.buf.as_slice::<u32>();
    if u32_slice.len() < 4 {
        log::error!("sid_from_msg: buffer too small ({} bytes, need 16)", msg.buf.len());
        return Err(xous::Error::InvalidArguments);
    }
    Ok(xous::SID::from_u32(u32_slice[0], u32_slice[1], u32_slice[2], u32_slice[3]))
}

fn name_from_msg(env: &MessageEnvelope, offset: usize) -> Result<&str, xous::Error> {
    let msg = env.body.memory_message().ok_or(xous::Error::InvalidArguments)?;
    let buf = msg.buf.as_slice::<u8>();
    // `valid` is required: callers must set it to the exact byte length of the name.
    // Defaulting to buf.len() on None would turn an empty or malformed request into
    // a huge NUL-filled slice that passes the UTF-8 check and poisons the name table.
    let valid_bytes = msg
        .valid
        .ok_or_else(|| {
            log::error!("name_from_msg: msg.valid is None; name length is required");
            xous::Error::InvalidString
        })?
        .get();
    // Reject names that exceed the documented maximum, before touching the buffer.
    if valid_bytes > api::NAME_MAX_LENGTH {
        log::error!(
            "name_from_msg: valid_bytes ({valid_bytes}) exceeds NAME_MAX_LENGTH ({})",
            api::NAME_MAX_LENGTH
        );
        return Err(xous::Error::InvalidString);
    }
    // Reject any combination where [offset .. offset + valid_bytes] extends outside
    // the buffer, including arithmetic overflow.
    let end = offset.checked_add(valid_bytes).filter(|&e| e <= buf.len()).ok_or_else(|| {
        log::error!(
            "name_from_msg: offset ({offset}) + valid_bytes ({valid_bytes}) \
             exceeds buffer length ({})",
            buf.len()
        );
        xous::Error::InvalidString
    })?;
    core::str::from_utf8(&buf[offset..end]).map_err(|_| xous::Error::InvalidString)
}

fn respond_error(mut msg: MessageEnvelope, error: xous::Error) {
    let mem = msg.body.memory_message_mut().unwrap();
    mem.buf.as_slice_mut::<u32>()[0] = 1;
    mem.buf.as_slice_mut::<u32>()[1] = error as u32;
    mem.valid = None;
    mem.offset = None;
}

fn respond_connect_success(mut msg: MessageEnvelope, cid: xous::CID) {
    let mem = msg.body.memory_message_mut().unwrap();
    mem.buf.as_slice_mut::<u32>()[0] = 0;
    mem.buf.as_slice_mut::<u32>()[1] = cid as u32;
    mem.valid = None;
    mem.offset = None;
}

fn respond_simple_success(mut msg: MessageEnvelope) {
    let mem = msg.body.memory_message_mut().unwrap();
    mem.buf.as_slice_mut::<u32>()[0] = 0;
    mem.valid = None;
    mem.offset = None;
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, HashSet};

    use app_manifest::{Locale, Manifest, Message, MessageType};

    use super::*;

    const APP_ID: [u8; app_manifest::APP_ID_BYTE_LEN] =
        [0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
    const OTHER_APP_ID: [u8; app_manifest::APP_ID_BYTE_LEN] =
        [0xff, 0xee, 0xdd, 0xcc, 0xbb, 0xaa, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x00];

    fn manifest_with_servers(servers: &[&str]) -> Manifest { manifest_with_app_id_servers(APP_ID, servers) }

    fn manifest_with_app_id_servers(
        app_id: [u8; app_manifest::APP_ID_BYTE_LEN],
        servers: &[&str],
    ) -> Manifest {
        let mut app_name = BTreeMap::new();
        app_name.insert(Locale("en".to_string()), "Test App".to_string());

        let mut manifest = Manifest {
            app_name,
            app_id,
            icon: None,
            publisher: None,
            description: None,
            version: None,
            servers: BTreeMap::new(),
            fixed_sids: BTreeMap::new(),
            permissions: BTreeMap::new(),
            memory: Vec::new(),
            syscall: Vec::new(),
            qr_match_rules: Vec::new(),
        };

        for (index, server_name) in servers.iter().enumerate() {
            let mut messages = BTreeMap::new();
            messages.insert(
                "Ping".to_string(),
                Message { id: index + 1, r#type: MessageType::Scalar, description: None, cfg: None },
            );
            manifest.servers.insert((*server_name).to_string(), messages);
        }

        manifest
    }

    fn set(values: &[&str]) -> HashSet<String> { values.iter().map(|value| (*value).to_string()).collect() }

    fn message_key(server_name: &str, message_name: &str) -> (String, String) {
        (server_name.to_string(), message_name.to_string())
    }

    #[test]
    fn process_manifest_servers_replaces_registration_grants_for_app_id() {
        let mut names = NameServer::default();
        let original_manifest = manifest_with_servers(&["kept/server", "removed/server"]);
        let refreshed_manifest = manifest_with_servers(&["kept/server"]);
        let app_id = AppId(original_manifest.app_id);

        names.process_manifest_servers(&original_manifest).unwrap();
        assert_eq!(
            names.register_permissions.get(&app_id).unwrap(),
            &set(&["kept/server", "removed/server"])
        );
        assert!(names.message_name_to_id.contains_key(&message_key("removed/server", "Ping")));

        names.process_manifest_servers(&refreshed_manifest).unwrap();

        assert_eq!(names.register_permissions.get(&app_id).unwrap(), &set(&["kept/server"]));
        assert!(!names.message_name_to_id.contains_key(&message_key("removed/server", "Ping")));
    }

    #[test]
    fn process_manifest_update_preserves_existing_state_when_permissions_are_invalid() {
        let mut names = NameServer::default();
        let mut original_manifest = manifest_with_servers(&["kept/server", "removed/server"]);
        original_manifest.permissions.insert("kept/server".to_string(), BTreeSet::from(["Ping".to_string()]));
        let mut invalid_refresh = manifest_with_servers(&["kept/server"]);
        invalid_refresh
            .permissions
            .insert("kept/server".to_string(), BTreeSet::from(["Missing".to_string()]));
        let app_id = AppId(original_manifest.app_id);

        names.process_manifest_update(&original_manifest).unwrap();

        assert_eq!(names.process_manifest_update(&invalid_refresh), Err(xous::Error::ServerNotFound));
        assert_eq!(
            names.register_permissions.get(&app_id).unwrap(),
            &set(&["kept/server", "removed/server"])
        );
        assert_eq!(names.server_owners.get("removed/server"), Some(&app_id));
        assert_eq!(names.message_name_to_id.get(&message_key("removed/server", "Ping")), Some(&vec![2]));
        assert_eq!(names.connect_permissions.get(&app_id).unwrap().get("kept/server"), Some(&vec![1]));
    }

    #[test]
    fn process_manifest_servers_rejects_server_name_owned_by_another_app() {
        let mut names = NameServer::default();
        let mut original_manifest = manifest_with_app_id_servers(APP_ID, &["os/settings"]);
        let mut colliding_manifest = manifest_with_app_id_servers(OTHER_APP_ID, &["os/settings"]);

        original_manifest.servers.get_mut("os/settings").unwrap().get_mut("Ping").unwrap().id = 74;
        colliding_manifest.servers.get_mut("os/settings").unwrap().get_mut("Ping").unwrap().id = 999;

        names.process_manifest_servers(&original_manifest).unwrap();

        assert_eq!(names.process_manifest_servers(&colliding_manifest), Err(xous::Error::MemoryInUse));
        assert_eq!(names.message_name_to_id.get(&message_key("os/settings", "Ping")), Some(&vec![74]));
        assert_eq!(names.server_owners.get("os/settings"), Some(&AppId(APP_ID)));
        assert!(!names.register_permissions.contains_key(&AppId(OTHER_APP_ID)));
    }

    #[test]
    fn process_manifest_servers_rejects_fixed_sid_collision() {
        let mut names = NameServer::default();
        let mut fixed_manifest = manifest_with_app_id_servers(APP_ID, &[]);
        let colliding_manifest = manifest_with_app_id_servers(OTHER_APP_ID, &["fixed/server"]);

        fixed_manifest.fixed_sids.insert("fixed/server".to_string(), "fixed-server-000".to_string());

        names.process_manifest_servers(&fixed_manifest).unwrap();

        assert_eq!(names.process_manifest_servers(&colliding_manifest), Err(xous::Error::MemoryInUse));
        assert_eq!(names.server_owners.get("fixed/server"), Some(&AppId(APP_ID)));
    }
}

fn main() -> ! { NameServer::default().run() }
