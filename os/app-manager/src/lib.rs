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
    LaunchError, RemoveThirdPartyCertificate, RemoveThirdPartyCertificateResult, ThirdPartyCertificateInfo,
    ThirdPartyCertificatesPage,
};
use app_manager::{
    GetAppIcon, GetAppName, GetInstalledApps, GetQrMatchRules, InstalledAppInfo, InstalledAppsPage,
    LaunchApp, LaunchAppBlocking, ListApps, SubscribeAppEvents,
};
use system_messages::{ChildCrashed, Disconnected};
use third_party_certs::ThirdPartyCertificateStore;

crypto::use_api!();
fs::use_api!();

#[cfg(not(keyos))]
use crate::launch::launch_app;
use crate::registry::AppRegistry;

const THIRD_PARTY_CERTIFICATE_PAGE_ITEMS: usize = 8;
const INSTALLED_APP_PAGE_ITEMS: usize = 8;

pub fn listen() { server::listen(AppManagerServer::new().unwrap()) }

#[derive(server::Server)]
#[name = "os/app-manager"]
pub struct AppManagerServer {
    app_event_subscribers: Vec<ArchiveEventSubscriber<AppEvent>>,
    app_registry: AppRegistry,
    third_party_cert_store: ThirdPartyCertificateStore,
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
        self.app_registry.qr_match_rules()
    }
}

impl BlockingArchiveHandler<GetInstalledApps> for AppManagerServer {
    fn handle(
        &mut self,
        msg: GetInstalledApps,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> InstalledAppsPage {
        // Apps can change on flash independently of app-manager, so reload before listing.
        if let Err(e) = self.app_registry.refresh_installed_apps() {
            log::warn!("GetInstalledApps: failed to refresh app registry, returning cached list: {e:?}");
        }
        installed_apps_page(
            self.app_registry.installed_apps(&msg.locale, &self.third_party_cert_store.trusted_publishers()),
            msg.offset,
            msg.limit,
        )
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
        msg: GetThirdPartyCertificates,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> ThirdPartyCertificatesPage {
        third_party_certificate_page(self.third_party_cert_store.list(), msg.offset, msg.limit)
    }
}

fn installed_apps_page(apps: Vec<InstalledAppInfo>, offset: usize, limit: usize) -> InstalledAppsPage {
    let total = apps.len();
    let offset = offset.min(total);
    let limit = limit.clamp(1, INSTALLED_APP_PAGE_ITEMS);
    let end = offset.saturating_add(limit).min(total);

    InstalledAppsPage { apps: apps[offset..end].to_vec(), next_offset: (end < total).then_some(end) }
}

fn third_party_certificate_page(
    certificates: Vec<ThirdPartyCertificateInfo>,
    offset: usize,
    limit: usize,
) -> ThirdPartyCertificatesPage {
    let total = certificates.len();
    let offset = offset.min(total);
    let limit = limit.clamp(1, THIRD_PARTY_CERTIFICATE_PAGE_ITEMS);
    let end = offset.saturating_add(limit).min(total);

    ThirdPartyCertificatesPage {
        certificates: certificates[offset..end].to_vec(),
        next_offset: (end < total).then_some(end),
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
        if let Err(e) = self.app_registry.refresh_installed_apps() {
            error!("failed to refresh installed apps before removing third-party certificate: {e:?}");
        }

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
        info!("PID {sender} is launching app 0x{}", hex::encode(app_id.0));

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
        info!("PID {sender} is asynchronously launching app 0x{}", hex::encode(app_id.0));
        if let Err(e) = self.launch_app(app_id, sender) {
            if let Some(s) = self.app_event_subscribers.iter().find(|s| s.pid() == sender) {
                let event = AppEvent::LaunchError { app_id: (&app_id).into(), error: e };
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
            GetAppName::ByAppId { id, locale } => self.app_registry.app_name_by_id(&id.into(), &locale),
            GetAppName::ByPid { pid, locale } => self.app_registry.app_name_by_pid(pid, &locale),
        }
    }
}

impl BlockingArchiveHandler<ListApps> for AppManagerServer {
    fn handle(
        &mut self,
        msg: ListApps,
        _sender: PID,
        _context: &mut ServerContext<Self>,
    ) -> Vec<app_manager::AppEntry> {
        self.app_registry.list_apps(&msg.locale, &msg.filter)
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
            error!("Failed to find launched_by PID for app ID 0x{}", hex::encode(app_id.0));
            return;
        };

        let event = AppEvent::AppCrashed {
            app_id: app_id.into(),
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
        let app_id_str = hex::encode(app_id.0);
        debug!("Launching app with ID: 0x{}", app_id_str);

        #[cfg(keyos)]
        if let Some(pid) = xous::app_id_to_pid(&app_id)? {
            log::debug!("App {:02x?} already running with pid {}", app_id, pid);
            self.app_registry.register_running_app(pid, app_id, sender);
            self.notify_app_launched(app_id, pid, sender);
            return Ok(pid);
        }

        // The app bundle may have been overwritten on disk while app-manager
        // stayed alive. Rescan before launch so the ELF path, manifest metadata,
        // resource root, and nameserver permissions all reflect the copied bundle.
        self.app_registry.refresh_installed_apps().map_err(|e| {
            log::error!("Failed to refresh installed apps before launching 0x{app_id_str}: {e:?}");
            LaunchError::InternalError
        })?;

        #[cfg(keyos)]
        let elf_path = self.app_registry.elf_path(app_id).ok_or(LaunchError::UnknownAppId)?;
        #[cfg(not(keyos))]
        let elf_path = self
            .app_registry
            .elf_path(app_id)
            .map(std::path::PathBuf::from)
            .ok_or(LaunchError::UnknownAppId)?;
        let check_trust =
            cfg!(feature = "production") || self.app_registry.requires_debug_signature_trust(app_id);
        // Imported developer certificates must never authorize a built-in AppId.
        // Production firmware still enforces Foundation signer trust via check_trust.
        let trusted_pubkeys = if self.app_registry.is_built_in_app(app_id) {
            Vec::new()
        } else {
            self.third_party_cert_store.trusted_pubkeys()
        };

        #[cfg(keyos)]
        let pid = {
            let verified_app = crate::launch::verify_app(&app_id, &elf_path, &trusted_pubkeys, check_trust)?;
            self.register_app_resources(app_id)?;
            verified_app.launch()?
        };

        #[cfg(not(keyos))]
        let pid = launch_app(&app_id, &elf_path, &trusted_pubkeys, check_trust)?;
        self.app_registry.register_running_app(pid, app_id, sender);

        self.notify_app_launched(app_id, pid, sender);

        Ok(pid)
    }

    fn notify_app_launched(&mut self, app_id: AppId, pid: PID, sender: PID) {
        let app_id_str = hex::encode(app_id.0);
        debug!("Notifying app launch for app ID: 0x{}", app_id_str);
        let event = AppEvent::AppLaunched { app_id: (&app_id).into(), pid, launched_by: sender };
        self.app_event_subscribers.retain(|s| s.send(&event).is_ok());
    }

    #[cfg(keyos)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(index: usize) -> ThirdPartyCertificateInfo {
        ThirdPartyCertificateInfo {
            name: format!("Publisher {index}"),
            company: "Example Company".to_string(),
            contact_email: "hello@example.com".to_string(),
            support_url: "https://example.com".to_string(),
            public_key: format!("{index:066x}"),
            not_before_unix_seconds: Some(0),
            not_after_unix_seconds: Some(u64::MAX),
            serial_number: index.to_string(),
            issuer: "issuer".to_string(),
            subject: "subject".to_string(),
            basic_constraints: "CA:FALSE".to_string(),
            key_usage: "Digital Signature".to_string(),
            extended_key_usage: "Code Signing".to_string(),
        }
    }

    fn installed_app(index: usize) -> InstalledAppInfo {
        InstalledAppInfo {
            app_id: format!("0x{index:032x}"),
            name: format!("App {index}"),
            bundled_icon_path: None,
            publisher: String::new(),
            version: String::new(),
            size_bytes: 0,
            description: String::new(),
            permissions: Vec::new(),
        }
    }

    #[test]
    fn installed_apps_page_caps_requested_limit() {
        let apps = (0..10).map(installed_app).collect::<Vec<_>>();

        let page = installed_apps_page(apps, 0, usize::MAX);

        assert_eq!(page.apps.len(), INSTALLED_APP_PAGE_ITEMS);
        assert_eq!(page.next_offset, Some(INSTALLED_APP_PAGE_ITEMS));
    }

    #[test]
    fn installed_apps_page_returns_tail() {
        let apps = (0..10).map(installed_app).collect::<Vec<_>>();

        let page = installed_apps_page(apps, 8, 8);

        assert_eq!(page.apps.len(), 2);
        assert_eq!(page.apps[0].name, "App 8");
        assert_eq!(page.next_offset, None);
    }

    #[test]
    fn third_party_certificate_page_caps_requested_limit() {
        let certificates = (0..10).map(cert).collect::<Vec<_>>();

        let page = third_party_certificate_page(certificates, 0, usize::MAX);

        assert_eq!(page.certificates.len(), THIRD_PARTY_CERTIFICATE_PAGE_ITEMS);
        assert_eq!(page.next_offset, Some(THIRD_PARTY_CERTIFICATE_PAGE_ITEMS));
    }

    #[test]
    fn third_party_certificate_page_returns_tail() {
        let certificates = (0..10).map(cert).collect::<Vec<_>>();

        let page = third_party_certificate_page(certificates, 8, 8);

        assert_eq!(page.certificates.len(), 2);
        assert_eq!(page.certificates[0].name, "Publisher 8");
        assert_eq!(page.next_offset, None);
    }
}
