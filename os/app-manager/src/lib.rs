// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

use log::{debug, error, info};
use server::{
    ArchiveEventSubscriber, ArchiveEventSubscriptionHandler, BlockingArchiveHandler, BlockingScalar,
    BlockingScalarHandler, MessageId as _, ScalarHandler, Server, ServerContext,
};
use xous::{AppId, SystemEvent, PID};

mod launch;
mod registry;
mod system_messages;
mod third_party_certs;

use app_manager::{
    AppEvent, GetThirdPartyCertificates, ImportThirdPartyCertificate, ImportThirdPartyCertificateResult,
    LaunchError, RemoveInstalledApp, RemoveInstalledAppResult, RemoveThirdPartyCertificate,
    RemoveThirdPartyCertificateResult, ThirdPartyCertificateInfo,
};
use app_manager::{
    GetAppIcon, GetAppName, GetQrMatchRules, InstalledAppInfo, LaunchApp, LaunchAppBlocking, ListApps,
    RefreshInstalledApps, SubscribeAppEvents,
};
use system_messages::{ChildCrashed, Disconnected};
use third_party_certs::ThirdPartyCertificateStore;

crypto::use_api!();
fs::use_api!();

#[cfg(not(keyos))]
use crate::launch::launch_app;
use crate::registry::AppRegistry;

pub fn listen() {
    #[cfg(not(keyos))]
    crate::launch::warn_if_app_elf_root_unset();
    server::listen(AppManagerServer::new().unwrap())
}

#[derive(server::Server)]
#[name = "os/app-manager"]
pub struct AppManagerServer {
    app_event_subscribers: Vec<ArchiveEventSubscriber<AppEvent>>,
    app_registry: AppRegistry,
    third_party_cert_store: ThirdPartyCertificateStore,
    names: server::xous_names::XousNames,
    panic_message_buf: xous::MemoryRange,
}

impl Default for AppManagerServer {
    fn default() -> Self {
        let panic_message_buf =
            xous::map_memory(None, None, 0x1000, xous::MemoryFlags::W | xous::MemoryFlags::POPULATE)
                .expect("Failed to allocate panic message buffer");

        Self {
            app_event_subscribers: Vec::default(),
            app_registry: AppRegistry::default(),
            third_party_cert_store: ThirdPartyCertificateStore::default(),
            names: server::xous_names::XousNames::new().expect("xous-names should be available at startup"),
            panic_message_buf,
        }
    }
}

impl BlockingArchiveHandler<GetQrMatchRules> for AppManagerServer {
    fn handle(
        &mut self,
        _msg: GetQrMatchRules,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Vec<app_manager::AppQrMatchRules> {
        self.app_registry.qr_match_rules(&self.third_party_cert_store.trusted_publishers())
    }
}

impl BlockingArchiveHandler<ListApps> for AppManagerServer {
    fn handle(
        &mut self,
        msg: ListApps,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Vec<InstalledAppInfo> {
        self.app_registry.list_apps(
            &msg.locale,
            &self.third_party_cert_store.trusted_publishers(),
            &msg.filter,
        )
    }
}

impl BlockingScalarHandler<RefreshInstalledApps> for AppManagerServer {
    fn handle(
        &mut self,
        _msg: RefreshInstalledApps,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), app_manager::AppManagerError> {
        self.app_registry.scan_installed_apps().map_err(|e| {
            error!("failed to refresh installed apps: {e:?}");
            app_manager::AppManagerError::InternalError
        })
    }
}

impl BlockingArchiveHandler<GetAppIcon> for AppManagerServer {
    fn handle(
        &mut self,
        msg: GetAppIcon,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Option<Vec<u8>> {
        let app_id = match app_manager::decode_app_id_str(&msg.app_id) {
            Ok(app_id) => app_id,
            Err(e) => {
                log::warn!("GetAppIcon: invalid app id {:?}: {e:?}", msg.app_id);
                return None;
            }
        };
        self.app_registry.app_icon_bytes(app_id)
    }
}

impl BlockingArchiveHandler<GetThirdPartyCertificates> for AppManagerServer {
    fn handle(
        &mut self,
        _msg: GetThirdPartyCertificates,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Vec<ThirdPartyCertificateInfo> {
        self.third_party_cert_store.list()
    }
}

impl BlockingArchiveHandler<ImportThirdPartyCertificate> for AppManagerServer {
    fn handle(
        &mut self,
        msg: ImportThirdPartyCertificate,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> ImportThirdPartyCertificateResult {
        match self.third_party_cert_store.import(&msg.certificate_pem) {
            Ok(cert) => ImportThirdPartyCertificateResult::Imported(cert),
            Err(()) => ImportThirdPartyCertificateResult::Invalid,
        }
    }
}

impl BlockingArchiveHandler<RemoveThirdPartyCertificate> for AppManagerServer {
    fn handle(
        &mut self,
        msg: RemoveThirdPartyCertificate,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> RemoveThirdPartyCertificateResult {
        // Only block removal while the certificate is still trusted. An expired cert can
        // no longer launch the app that was signed with it, so the user must be able to
        // delete the stale entry even though an installed app still references that key.
        if self.third_party_cert_store.is_trusted(&msg.public_key) {
            if let Some(app_name) =
                self.app_registry.app_name_requiring_third_party_key(&msg.public_key, &msg.locale)
            {
                return RemoveThirdPartyCertificateResult::AppRequiresKey(app_name);
            }
        }

        match self.third_party_cert_store.remove(&msg.public_key) {
            Ok(true) => RemoveThirdPartyCertificateResult::Removed,
            Ok(false) => RemoveThirdPartyCertificateResult::NotFound,
            Err(e) => {
                error!("failed to remove third-party certificate: {e:?}");
                RemoveThirdPartyCertificateResult::InternalError
            }
        }
    }
}

impl BlockingArchiveHandler<RemoveInstalledApp> for AppManagerServer {
    fn handle(
        &mut self,
        msg: RemoveInstalledApp,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> RemoveInstalledAppResult {
        let app_id = match app_manager::decode_app_id_str(&msg.app_id) {
            Ok(app_id) => app_id,
            Err(e) => {
                log::warn!("RemoveInstalledApp: invalid app id {:?}: {e:?}", msg.app_id);
                return RemoveInstalledAppResult::NotFound;
            }
        };

        if self.app_registry.is_running(&app_id) {
            return RemoveInstalledAppResult::Running;
        }

        let Some(app_dir) = self.app_registry.sideloaded_bundle_dir(app_id) else {
            return if self.app_registry.contains_app(app_id) {
                RemoveInstalledAppResult::NotSideloaded
            } else {
                RemoveInstalledAppResult::NotFound
            };
        };

        match FileSystem::default().remove(&app_dir, fs::Location::System) {
            Ok(()) | Err(fs::Error::FileNotFound) => {}
            Err(e) => {
                error!("failed to remove sideloaded app bundle {app_dir}: {e:?}");
                return RemoveInstalledAppResult::InternalError;
            }
        }

        self.app_registry.clear_registered_manifest(app_id);

        match self.app_registry.scan_installed_apps() {
            Ok(()) => RemoveInstalledAppResult::Removed,
            Err(e) => {
                error!("failed to refresh installed apps after removing {app_dir}: {e:?}");
                RemoveInstalledAppResult::InternalError
            }
        }
    }
}

impl Server for AppManagerServer {
    fn on_start(&mut self, context: &mut ServerContext<Self>) {
        self.app_registry.scan_installed_apps().expect("Failed to scan installed apps");

        xous::register_system_event_handler(SystemEvent::ChildTerminated, context.sid(), ChildCrashed::ID)
            .expect("Failed to register child terminated handler");
        xous::register_system_event_handler(SystemEvent::Disconnected, context.sid(), Disconnected::ID)
            .expect("Failed to register disconnected handler");
    }
}

impl AppManagerServer {
    pub fn new() -> anyhow::Result<Self> { Ok(Self::default()) }
}

impl BlockingScalarHandler<LaunchAppBlocking> for AppManagerServer {
    fn handle(
        &mut self,
        LaunchAppBlocking(app_id): LaunchAppBlocking,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> <LaunchAppBlocking as BlockingScalar>::Response {
        info!("PID {sender} is launching app 0x{app_id}");

        let pid = self.launch_app(app_id, sender)?;
        Ok(pid)
    }
}

impl ArchiveEventSubscriptionHandler<SubscribeAppEvents> for AppManagerServer {
    fn handle(
        &mut self,
        _msg: SubscribeAppEvents,
        subscriber: ArchiveEventSubscriber<AppEvent>,
        _context: &mut ServerContext<Self>,
    ) -> Result<(), server::Infallible> {
        debug!("New app event subscriber: {:?}", subscriber);

        self.app_event_subscribers.push(subscriber);
        Ok(())
    }
}

impl ScalarHandler<LaunchApp> for AppManagerServer {
    fn handle(&mut self, LaunchApp(app_id): LaunchApp, sender: PID, _context: &mut ServerContext<Self>) {
        info!("PID {sender} is asynchronously launching app 0x{app_id}");
        if let Err(e) = self.launch_app(app_id, sender) {
            if let Some(s) = self.app_event_subscribers.iter().find(|s| s.pid() == sender) {
                let event = AppEvent::LaunchError { app_id, error: e };
                if s.send(&event).is_err() {
                    error!("Failed to send launch error to subscriber PID {sender}");
                }
            }
        }
    }
}

impl BlockingArchiveHandler<GetAppName> for AppManagerServer {
    fn handle(
        &mut self,
        msg: GetAppName,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Option<String> {
        match msg {
            GetAppName::ByAppId { id, locale } => self.app_registry.app_name_by_id(&id, &locale),
            GetAppName::ByPid { pid, locale } => self.app_registry.app_name_by_pid(pid, &locale),
        }
    }
}

impl ScalarHandler<ChildCrashed> for AppManagerServer {
    fn handle(
        &mut self,
        ChildCrashed(exit_code): ChildCrashed,
        sender: PID,
        _context: &mut ServerContext<Self>,
    ) {
        let Some(app_id) = self.app_registry.app_id_by_pid(sender) else {
            error!("Failed to get app ID for PID {sender}");
            return;
        };

        let Some(launched_by) = self.app_registry.launched_by(app_id) else {
            error!("Failed to find launched_by PID for app ID 0x{app_id}");
            return;
        };

        let event = AppEvent::AppCrashed {
            app_id: *app_id,
            pid: sender,
            launched_by,
            exit_code,
            panic_message: if exit_code != 0 { self.read_panic_message(sender) } else { None },
        };
        self.app_event_subscribers.retain(|s| s.send(&event).is_ok());

        self.app_registry.terminate_app(sender);
    }
}

impl ScalarHandler<Disconnected> for AppManagerServer {
    fn handle(&mut self, _: Disconnected, sender: PID, _context: &mut ServerContext<Self>) {
        self.app_event_subscribers.retain(|s| s.pid() != sender);
    }
}

impl AppManagerServer {
    fn launch_app(&mut self, app_id: AppId, sender: PID) -> Result<PID, LaunchError> {
        debug!("Launching app with ID: 0x{app_id}");

        #[cfg(keyos)]
        if let Some(pid) = xous::app_id_to_pid(&app_id)? {
            log::debug!("App 0x{app_id} already running with pid {pid}");
            self.app_registry.register_running_app(pid, app_id, sender);
            self.notify_app_launched(app_id, pid, sender);
            return Ok(pid);
        }

        // The registry tracks the bundle's fs `app.elf` path; the device launches
        // it from the image, the simulator from the staged host binary.
        let elf_path = self.app_registry.elf_path(app_id).ok_or(LaunchError::UnknownAppId)?;

        // Trust is dynamic: a sideloaded app launches only while its signer matches a
        // currently-valid publisher cert, so importing or removing a cert takes effect
        // without a rescan. Built-in and hosted apps are always launchable.
        if !self.app_registry.is_launchable(app_id, &self.third_party_cert_store.trusted_publishers()) {
            return Err(LaunchError::UntrustedPublisher);
        }

        // Register names and resources only after the trust check, so an untrusted app never
        // claims server names or resource access.
        let manifest_bytes = self.app_registry.manifest_bytes(app_id).ok_or(LaunchError::UnknownAppId)?;
        self.names.add_manifest(manifest_bytes).map_err(|e| {
            error!("could not register manifest names for app 0x{app_id}: {e:?}");
            LaunchError::NameRegistration
        })?;
        self.register_app_resources(app_id)?;

        // The manifest's signed file hashes guard app.elf and the resources at launch; on the
        // simulator nothing is signed, so the staged host binary just runs.
        #[cfg(keyos)]
        let pid = {
            let file_hashes = self.app_registry.file_hashes(app_id).ok_or(LaunchError::UnknownAppId)?;
            let signer = self.app_registry.elf_signer(app_id);
            crate::launch::verify_and_launch(&app_id, &elf_path, &file_hashes, signer)?
        };
        #[cfg(not(keyos))]
        let pid = {
            let host_elf = crate::launch::host_elf_path(&elf_path).ok_or(LaunchError::UnknownAppId)?;
            launch_app(&app_id, &host_elf)?
        };

        self.app_registry.register_running_app(pid, app_id, sender);

        self.notify_app_launched(app_id, pid, sender);

        Ok(pid)
    }

    fn notify_app_launched(&mut self, app_id: AppId, pid: PID, sender: PID) {
        debug!("Notifying app launch for app ID: 0x{app_id}");
        let event = AppEvent::AppLaunched { app_id, pid, launched_by: sender };
        self.app_event_subscribers.retain(|s| s.send(&event).is_ok());
    }

    fn register_app_resources(&self, app_id: AppId) -> Result<(), LaunchError> {
        if let Some(location) = self.app_registry.app_resources_location(app_id) {
            FileSystem::default()
                .register_app_resources(app_id, location.root, location.app_dir)
                .map_err(|_| LaunchError::InternalError)?;
        }

        Ok(())
    }

    fn read_panic_message(&mut self, child_pid: PID) -> Option<String> {
        if let Ok((panic_pid, panic_size)) = xous::get_panic_message(self.panic_message_buf) {
            if xous::PID::new(panic_pid) == Some(child_pid) {
                return String::from_utf8(self.panic_message_buf.as_slice()[..panic_size].to_owned()).ok();
            }
            log::debug!("Panic message PID mismatch: expected {child_pid}, got {panic_pid}");
        }
        None
    }
}
