<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Permission Groups Draft

Draft only. Initially built from the API manifests under `api/*/manifest.toml` on 2026-05-20. Last checked on 2026-06-05: the manifests contain 24 servers and 334 message entries, excluding `api/docs` because it has no message manifest. The detailed map still needs four current messages classified.

The goal here is not to expose crate names directly to users. The OS should group by consequence and resource, then retain `server/message` as technical detail below the fold.

## Engineering Overview

Kiosk permissions should be modeled as policy metadata on message sends, with a small number of additional concepts for scoped and temporary grants. The goal is to make permissions understandable to users without forcing the kernel to understand every domain-specific resource type in the system.

The current API manifests list raw `server/message` permissions. That is too low-level for app install review: an app may request many messages, and users cannot safely infer consequence from message names alone. The proposed model keeps message-level enforcement as the base layer, but adds structured metadata so the Apps page, installer, permission broker, and eventually the kernel/nameserver can reason about the request consistently.

Each permission should be described along separate axes:

- **Risk**: what can go wrong if this is allowed.
- **Sender policy**: which class of sender may ever send the message.
- **Grant timing**: when and how the user approves it.
- **User-facing group**: where it appears in the permission UI.
- **Optional scope**: the domain-specific resource being granted, such as a file location, camera session, BLE device, USB endpoint, or NFC operation.

These axes should stay separate. For example, `signed` is not the same kind of thing as `grant-on-first-use`, and `developer` should not be ORed together with trust levels in a way that makes "Foundation-signed in Developer Mode" ambiguous. If a permission needs multiple conditions, use separate fields such as `required_trust` and `required_mode`.

Sender policy is a trust hierarchy:

```text
static-server => foundation-signed => signed
```

`signed` means any signed dynamically installable app, including Foundation apps. `foundation-signed` means a dynamically installable Foundation app or anything stronger. `static-server` means a non-dynamically-loadable Foundation server shipped in the OS image. `developer` remains a separate developer/simulator-only mode.

The kernel should not parse domain-specific message payloads. It should not need to understand file paths, BLE UUIDs, NFC records, setting keys, or USB endpoint layouts. Those checks belong to the server that owns the resource. The kernel should enforce message permission, sender identity, and possibly opaque grant ownership/lifetime. Servers and brokers should enforce resource semantics.

For scoped grants, use persistent server-owned grant records plus optional ephemeral handles. A persisted filesystem permission might say "app X may read this user-selected location." At runtime, the FS server can check caller identity and path semantics directly, or mint an opaque session handle for brokered/file-picker flows. Apps should not persist runtime handles across reboot; handles are useful for temporary, scoped, revocable, or brokered access, while persistent grants live in the permission database or owning server.

The first implementation pass should not try to solve every scoped-resource problem in the kernel. A practical sequence is:

1. Add permission metadata to manifests.
2. Add a validator that rejects missing metadata, invalid sender policies, and forbidden dynamic grants.
3. Update install/app-details UI to group and explain permissions from metadata.
4. Keep existing server-side checks for scoped resources.
5. Add grant-on-first-use brokers for camera, NFC, BLE, USB, and file-picker style flows.
6. Add revocable or temporary grant support only where `while-active` behavior is actually required.

This keeps the model small enough to implement while leaving room for stronger kernel-assisted grants later.

## Risk Levels

| Risk      | Meaning                                                                                                                               | Suggested grant behavior                                                         |
| --------- | ------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- |
| Standard  | Low-risk status, UI drawing, haptics, or public information.                                                                          | Can be enabled by group. Show in install summary.                                |
| Sensitive | Personal data, app data, device identifiers, network sync, or local files.                                                            | Enabled only from install review or just-in-time prompt.                         |
| High      | Can impersonate the user, move money-related data, alter security posture, or expose broad device state.                              | Per-permission switch. Avoid group-level silent enable.                          |
| Critical  | Seed material, PIN material, destructive storage actions, firmware install, screen/input control, raw storage, or system file access. | Default off. Require explicit review, likely ask-every-time or first-party only. |
| Internal  | Boot, simulator, debug, bus control, DMA, or privileged OS plumbing.                                                                  | Hide from signed-app UI unless developer mode is active.                         |

## Sender Policy

Risk describes consequence. Sender policy describes which class of sender may ever send the message. The receiver is implicit in the `server/message` pair. These should be separate fields in manifest metadata.

| Sender policy | Meaning | Suggested enforcement |
| --- | --- | --- |
| `signed` | Any signed dynamically installable app may request this message if the user approves it. This includes Foundation-signed apps. | Manifest request + user grant. Risk and grant timing decide how visible the review is. |
| `foundation-signed` | Only Foundation-signed apps may request this message. A static server satisfies this too, because static-server implies Foundation trust. | Publisher/signature gate before any user grant. |
| `static-server` | Only a static, non-dynamically-loadable Foundation server shipped in the OS image may send this message. | Static server identity / image membership gate; not grantable to dynamic installs. |
| `developer` | Only available when Developer Mode or simulator/test configuration is active. | Deny in production app review; show only in developer diagnostics. |

Sender policy values are ordered trust requirements, not independent identities: `static-server` implies `foundation-signed`, and `foundation-signed` implies `signed`. Each message should still list one policy, using the narrowest required sender class. The previous split between `third-party-grantable` and `third-party-review` is better represented as `signed` plus Risk and Grant timing. In other words, sender policy answers "can this kind of signed app ever send it?", while Risk and Grant timing answer "how hard should the user have to look before approving it?"

Recommendation: reserve `static-server` for PIN entry/management, master seed read/write, firmware installation, system storage, raw buses, and trusted publisher management. A Foundation-signed dynamic app can be trusted more than an arbitrary signed app, but it is still part of the dynamic app supply chain, so it should not automatically be able to send the same privileged messages as code shipped in the OS image.

Use `foundation-signed` sparingly. It is useful only when dynamic Foundation apps should be eligible but arbitrary signed apps should not. If dynamic delivery is too much trust for a permission, use `static-server`.

## Grant Timing Policy

Sender policy says whether an app may ever send a message. Grant timing says when and how the user approves that ability.

| Grant timing         | Meaning                                                                                 | Good fits                                                                                      |
| -------------------- | --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `install-review`     | Approved during install or in the app's permissions page.                               | Static capabilities that are easy to understand without immediate context.                     |
| `grant-on-first-use` | Prompt the first time the app tries to use the capability, then persist unless revoked. | Camera, NFC, BLE, location-like filesystem access, and device identity reads. |
| `while-active`       | Temporary foreground/session grant, usually paired with first use.                      | Camera streaming, NFC scanning, BLE data exchange.                                             |
| `grant-on-each-use`  | Ask for every operation or every sensitive transaction.                                 | Signing, destructive actions, and critical exports.                                            |
| `location-grant`     | Grant is scoped to a filesystem location and operation, not raw file IPC.               | User files, USB drive, Airlock.                                                                |
| `policy-only`        | No user prompt. The app either passes Sender policy or it does not.                     | `static-server`, `foundation-signed`, and `developer` permissions.                               |

Recommendation: camera, NFC data, and BLE data should be `grant-on-first-use` plus `while-active` by default. The install page can still disclose that the app may ask for them later, but the actual grant should happen in context.

First-use grants should be scoped to the concrete resource and operation, not the broad subsystem. For example, camera grants should be tied to active foreground capture, NFC grants should distinguish read from write, BLE grants should be tied to the selected device/service where possible, and USB grants should be tied to the claimed device/interface/endpoint.

## Primary UI Groups

These are the groups I would show on the Apps page. The subgroups in the message map are implementation tags inside each primary group.

| Primary group         | User-facing label           | Default posture                                                                                                                     |
| --------------------- | --------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `app-management`      | Apps and publisher trust    | Standard for app listing/launching, High for developer certificates.                                                                |
| `ui-and-input`        | Interface and navigation    | Standard for app drawing, High/Critical for navigation control, capture, and input injection.                                       |
| `file-system`         | File system                 | Sensitive by location, Critical for system locations, raw blocks, and formatting.                                                   |
| `device-secrets`      | Device secrets and identity | Critical by default. App-scoped seed is available to signed apps; master seed and PIN are never dynamically grantable. |
| `backup-and-recovery` | Backup and recovery         | High/Critical. Per-action review.                                                                                                   |
| `cryptography`        | Cryptography                | Sensitive for primitives, High/Critical for signing or seed-derived material.                                                       |
| `network-and-pairing` | Network sync and pairing    | Sensitive/High depending on whether it sends wallet data or only reads status.                                                      |
| `device-connectivity` | Bluetooth, NFC, and USB     | Sensitive for data exchange, High for device emulation and raw endpoint access.                                                     |
| `peripherals`         | Peripherals                 | Mostly Internal. Some visible controls can be Sensitive.                                                                            |
| `settings`            | Settings                    | Standard for read/subscribe, Sensitive/High for writes that change security or radios.                                              |
| `power-and-firmware`  | Power and firmware          | High/Critical. Static-server only for most dynamic apps.                                                                            |
| `developer`           | Developer controls          | Hidden unless Developer Mode is active.                                                                                             |

## Message Map

### `app-management`

| Subgroup                         | Risk               | Sender policy  | Grant timing         | Messages                                                                                                    |
| -------------------------------- | ------------------ | -------------- | -------------------- | ----------------------------------------------------------------------------------------------------------- |
| `app-management.discovery`       | Standard/Sensitive | `signed`  | `install-review`     | `os/app-manager`: `GetAppName`, `GetQrAcceptanceCriteria`, `GetInstalledApps`              |
| `app-management.lifecycle`       | Sensitive/High     | `signed`  | `grant-on-first-use` | `os/app-manager`: `LaunchAppBlocking`, `LaunchApp`, `SubscribeAppEvents`                                    |
| `app-management.publisher-trust` | High/Critical      | `static-server` | `policy-only`        | `os/app-manager`: `GetThirdPartyCertificates`, `ImportThirdPartyCertificate`, `RemoveThirdPartyCertificate` |

### `ui-and-input`

| Subgroup                             | Risk              | Sender policy    | Grant timing         | Messages                                                                                                                                                            |
| ------------------------------------ | ----------------- | ---------------- | -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `ui-and-input.app-surface`           | Standard          | `signed`    | `install-review`     | `os/gui-server`: `RegisterAppMessage`, `SubmitFrame`, `RequestRedraw`, `AnimateNextFrame`, `UpdateKeyboard`, `HideKeyboard`, `ShowModal`             |
| `ui-and-input.wake-lock`             | Sensitive         | `signed`    | `grant-on-first-use` | `os/gui-server`: `SetWakeLock`                                                                                                                                      |
| `ui-and-input.navigation`            | Sensitive/High    | `signed`    | `install-review`     | `os/gui-server`: `SwitchTo`, `SwitchToLauncher`, `CloseApp`, `NavigateTo`, `FinishResponse`, `NavigationCancel`, `GetPendingNavRequest`, `ShowCamera`, `HideCamera` |
| `ui-and-input.privileged-navigation` | High/Critical     | `static-server`   | `policy-only`        | `os/gui-server`: `LoginSuccess`, `ShowControlCenter`, `RunApp`                                                                                                      |
| `ui-and-input.system-key-events`     | High/Internal     | `static-server` | `policy-only`        | `os/gui-server`: `KeyPressed`, `KeyReleased`                                                                                                                        |
| `ui-and-input.capture-and-injection` | Critical/Internal | `static-server` | `policy-only`        | `os/gui-server`: `CaptureScreen`, `InjectTouch`, `InjectKey`, `Shutdown`                                                                                            |
| `developer.simulator-ui`             | Developer-only    | `developer`      | `policy-only`        | `os/gui-server`: `GetDeviceFrame`, `SetScaleFactor`, `SimulatePowerButton`, `SimulateKey`, `SimulateScroll`                                                         |

### `file-system`

| Subgroup                                 | Risk                                  | Sender policy    | Grant timing         | Messages                                                                                                                                                                                                                    |
| ---------------------------------------- | ------------------------------------- | ---------------- | -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `file-system.file-handles`               | Standard/Sensitive, inherits location | `signed`    | `location-grant`     | `os/fs`: `OpenFileMessage`, `CloseFile`, `OpenDirMessage`, `CloseDir`, `SeekFile`, `Flush`, `FlushFs`, `MapFileMessage`, `GetMetadata`, `NextEntry`, `SubscribeFilesystemEvent`, `SetMtime`                                 |
| `file-system.read-write`                 | Sensitive, inherits location          | `signed`    | `location-grant`     | `os/fs`: `ReadFile`, `WriteFile`, `AsyncRead`, `AsyncWrite`, `AsyncCopyBlock`                                                                                                                                               |
| `file-system.mutate-delete`              | Sensitive/High, inherits location     | `signed`    | `location-grant`     | `os/fs`: `Remove`, `AtomicCopy`, `CreateDirMessage`, `Rename`, `TruncateFile`, `SetLen`                                                                                                                                     |
| `file-system.raw-blocks`                 | Critical/Internal                     | `static-server` | `policy-only`        | `os/fs`: `ReadBlocks`, `WriteBlocks`, `BlockCount`                                                                                                                                                                          |
| `file-system.volume-admin`               | Critical/Internal                     | `static-server`   | `policy-only`        | `os/fs`: `DiskEncryptionKeysReady`, `FormatEncryptedVolume`, `MountAirlock`, `FormatAirlock`, `FormatUsb`                                                                                                                   |
| `file-system.app-resource-registration`  | Sensitive/High                        | `static-server`   | `policy-only`        | `os/fs`: `RegisterAppResources`                                                                                                                                                                                             |
| `file-system.user-usb-airlock-locations` | Sensitive                             | `signed`    | `grant-on-first-use` | `os/fs`: `GetUsbReadAccess`, `GetUsbWriteAccess`, `GetUserReadAccess`, `GetUserWriteAccess`, `GetAirlockReadAccess`, `GetAirlockWriteAccess`                                                                                |
| `file-system.system-locations`           | Critical/Internal                     | `static-server`   | `policy-only`        | `os/fs`: `GetBootReadAccess`, `GetBootWriteAccess`, `GetEncryptedRootReadAccess`, `GetEncryptedRootWriteAccess`, `GetSystemReadAccess`, `GetSystemWriteAccess`, `GetSystemAppDataReadAccess`, `GetSystemAppDataWriteAccess` |

### `device-secrets`

| Subgroup                                 | Risk               | Sender policy    | Grant timing                                | Messages                                                                                                                                                                                   |
| ---------------------------------------- | ------------------ | ---------------- | ------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `device-secrets.owner-auth`              | Critical           | `static-server`   | `policy-only`                               | `os/security`: `SetSeedAndPin`, `ChangePin`, `Login`, `Logout`, `GetPin`, `Lockout`, `GetSecurityWords`                                                                                    |
| `device-secrets.master-seed`             | Critical           | `static-server`   | `policy-only`                               | `os/security`: `GetSeed`, `SetSeed`                                                                                                                                                        |
| `device-secrets.app-scoped-seed`         | Sensitive          | `signed`    | `install-review`                            | `os/security`: `GetAppSeed`                                                                                                                                                                |
| `device-secrets.device-identity`         | Sensitive/High     | `signed`    | `grant-on-first-use`                        | `os/security`: `GetDeviceId`, `GetSeedFingerprint`, `ComputeSeedFingerprint`                                                                                                               |
| `device-secrets.status`                  | Standard/Sensitive | `signed`    | `install-review`                            | `os/security`: `GetAttemptsRemaining`, `GetFactoryResetCounter`, `LoggedIn`, `IsPinSet`, `GetPinEntryMode`, `GetMasterKeyState`, `GetOsVersionInfo`, `GetBootloaderBuildDate`, `GetRandom` |
| `device-secrets.firmware-timestamp`      | Sensitive/High     | `foundation-signed` | `policy-only`                               | `os/security`: `GetFirmwareTimestamp`, `SetFirmwareTimestamp`                                                                                                                              |
| `device-secrets.admin-and-pairing-state` | High/Internal      | `static-server`   | `policy-only`                               | `os/security`: `SetAttempts`, `GetBluetoothChallengeSecret`, `SetBluetoothCheckSecretSent`, `SetBluetoothDeviceId`                                                                         |

### `backup-and-recovery`

| Subgroup                             | Risk           | Sender policy  | Grant timing  | Messages                                                                                                                                                                                                                                                                          |
| ------------------------------------ | -------------- | -------------- | ------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `backup-and-recovery.local-backup`   | High/Critical  | `static-server` | `policy-only` | `os/backup`: `StatusSubscribe`, `CreateBackup`, `RestoreBackup`, `SubscribeRestoreProgress`, `CreateBackupFile`                                                                                                                                                                   |
| `backup-and-recovery.keycard.status` | Sensitive/High | `static-server` | `policy-only` | `os/keycard`: `IdentifyKeycard`, `DetectKeycard`, `CheckBackup`                                                                                                                                                                                                                   |
| `backup-and-recovery.keycard.read`   | High/Critical  | `static-server` | `policy-only` | `os/keycard`: `PopShard`, `LoadShardFromKeycard`                                                                                                                                                                                                                                  |
| `backup-and-recovery.keycard.write`  | High/Critical  | `static-server` | `policy-only` | `os/keycard`: `ResetShards`, `GenerateShards`, `PushShard`, `StoreShardToKeycard`, `RestoreMasterSeed`, `SetShamirScheme`, `FormatKeycard`                                                                                                                                        |
| `backup-and-recovery.magic-backup`   | High/Critical  | `static-server` | `policy-only` | `os/quantum-link`: `BackupShard`, `RestoreShard`, `EnvoyMagicBackupEnabled`, `SendMagicBackupEvent`, `SendRestoreMagicBackupResult`, `AwaitCreateMagicBackupResult`, `StartRestoreMagicBackup`, `MagicBackupStatus`, `SubscribeRestoreMagicBackup`, `SendPrimeMagicBackupEnabled` |

### `cryptography`

| Subgroup                       | Risk              | Sender policy    | Grant timing        | Messages                                                                                                                                   |
| ------------------------------ | ----------------- | ---------------- | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `cryptography.primitives`      | Sensitive         | `signed`    | `install-review`    | `os/crypto`: `AesSetup`, `AesExecute`, `AesAad`, `AesGcmTag`, `AesClear`, `Hmac`, `ShaSetContext`, `ShaUpdate`, `ShaGetContext`, `ShaDrop` |
| `cryptography.secret-sharing`  | High/Critical     | `signed`    | `grant-on-each-use` | `os/crypto`: `ShamirSplit`, `ShamirRecover`                                                                                                |
| `cryptography.disk-encryption` | Critical/Internal | `static-server` | `policy-only`       | `os/crypto`: `DiskEncryptUnsafe`, `SubscribeDiskEncryptComplete`                                                                           |
| `cryptography.secure-signing`  | High/Critical     | `static-server`   | `policy-only`       | `os/security`: `SignWithSecurityCheckKey`, `SignWithFidoKey`, `GetFidoPubkey`, `ScChallenge`, `KeycardAuthenticityMac`                     |

### `network-and-pairing`

| Subgroup                                        | Risk               | Sender policy    | Grant timing         | Messages                                                                                                                                                                                  |
| ----------------------------------------------- | ------------------ | ---------------- | -------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `network-and-pairing.prestart`                  | Internal           | `static-server`   | `policy-only`        | `os/ql-prestart`: `StartWithoutFilesystem`                                                                                                                                                |
| `network-and-pairing.device-link-status`        | Standard/Sensitive | `signed`    | `install-review`     | `os/quantum-link`: `SubscribeConnectionStatus`                                                                                                                                            |
| `network-and-pairing.pairing-control`           | High               | `foundation-signed` | `policy-only`        | `os/quantum-link`: `GetXidDocument`, `SubscribePairingEvent`, `ClearPairedDevice`                                                                                                         |
| `network-and-pairing.wallet-sync`               | Sensitive/High     | `signed`    | `grant-on-first-use` | `os/quantum-link`: `PublishPsbt`, `SendAccountUpdate`, `SubscribeSignPsbt`, `SubscribeAccountUpdate`, `SubscribePublishedAccountUpdate`, `SendApplyPassphrase`, `SendPrimeFiatPreference` |
| `network-and-pairing.status-and-rates`          | Standard/Sensitive | `signed`    | `install-review`     | `os/quantum-link`: `SubscribeExchangeRate`, `SubscribeExchangeRateHistory`, `SubscribeEnvoyStatus`, `EnvoyTimezone`                                                                       |
| `network-and-pairing.remote-firmware`           | High/Critical      | `static-server`   | `policy-only`        | `os/quantum-link`: `SubscribeFirmwareFetch`, `CheckFirmwareUpdate`, `StartFirmwareUpdate`, `NotifyFirmwareInstall`                                                                        |
| `network-and-pairing.onboarding-security-check` | Sensitive/Internal | `static-server`   | `policy-only`        | `os/quantum-link`: `NotifyOnboardingState`, `SubscribeOnboardingState`, `SubscribeSecurityCheckState`                                                                                     |

### `device-connectivity`

| Subgroup                                   | Risk               | Sender policy    | Grant timing                         | Messages                                                                                                                                              |
| ------------------------------------------ | ------------------ | ---------------- | ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| `device-connectivity.bluetooth-status`     | Standard/Sensitive | `signed`    | `install-review`                     | `os/bt`: `GetBtAddr`, `GetState`, `SubscribeBleState`, `GetBleVersionInfo`                                                                            |
| `device-connectivity.bluetooth-control`    | Sensitive/High     | `foundation-signed` | `policy-only`                        | `os/bt`: `EnableBle`, `DisableBle`, `Reset`, `Poll`, `DisableAdvChannels`, `Disconnect`                                                               |
| `device-connectivity.bluetooth-data`       | High               | `signed`    | `grant-on-first-use`; `while-active` | `os/bt`: `SubscribeBle`, `SendBle`                                                                                                                    |
| `developer.bluetooth-test`                 | Developer-only     | `developer`      | `policy-only`                        | `os/bt`: `TestEcho`                                                                                                                                   |
| `device-connectivity.nfc-status`           | Standard/Sensitive | `signed`    | `install-review`                     | `os/nfc`: `IsEnabled`, `IsActive`                                                                                                                     |
| `device-connectivity.nfc-data`             | Sensitive/High     | `signed`    | `grant-on-first-use`; `while-active` | `os/nfc`: `ReadNdefRawMsg`, `WriteNdefRawMsg`                                                                                                         |
| `device-connectivity.nfc-control`          | Sensitive/High     | `foundation-signed` | `policy-only`                        | `os/nfc`: `SetEnabled`                                                                                                                                |
| `device-connectivity.usb-host-status`      | Standard/Sensitive | `signed`    | `install-review`                     | `os/usb`: `Subscribe`, `IsEnabled`, `IsConnected`                                                                                                     |
| `device-connectivity.usb-host-data`        | Sensitive/High     | `signed`    | `grant-on-first-use`                 | `os/usb`: `Claim`, `OpenEndpoint`, `BulkOut`, `BulkIn`                                                                                                |
| `device-connectivity.usb-host-control`     | Sensitive/High     | `foundation-signed` | `policy-only`                        | `os/usb`: `SetEnabled`                                                                                                                                |
| `device-connectivity.usb-device-status`    | Standard/Sensitive | `signed`    | `install-review`                     | `os/usbdev`: `NumInterfaces`, `IsDeviceEmulationEnabled`, `IsDeviceEmulationConnected`, `IsCableConnected`, `IsDeviceMode`                            |
| `device-connectivity.usb-device-emulation` | High/Critical      | `signed`    | `grant-on-first-use`                 | `os/usbdev`: `RegisterInterface`, `WaitForConnection`, `ReadEndpoint`, `WriteEndpoint`, `RegisterSetupResponder`, `RegisterCapability`, `SetupPacket` |
| `device-connectivity.usb-device-control`   | High/Critical      | `foundation-signed` | `policy-only`                        | `os/usbdev`: `SetEndpointStalled`, `SetVidPid`, `ResetController`                                                                                     |

### `peripherals`

| Subgroup                     | Risk               | Sender policy    | Grant timing                         | Messages                                                                                                                                                          |
| ---------------------------- | ------------------ | ---------------- | ------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `peripherals.camera-status`  | Standard/Sensitive | `signed`    | `install-review`                     | `os/camera`: `IsEnabled`, `IsInUse`                                                                                                                               |
| `peripherals.camera-use`     | Sensitive/High     | `signed`    | `grant-on-first-use`; `while-active` | `os/camera`: `Subscribe`, `NotifyVisible`, `GetParams`, `SetParams`                                                                                               |
| `peripherals.camera-control` | Sensitive/High     | `foundation-signed` | `policy-only`                        | `os/camera`: `SetEnabled`                                                                                                                                         |
| `peripherals.gpio`           | Internal           | `static-server` | `policy-only`                        | `os/gpio`: `ClaimPin`, `EnableIrq`, `SetPin`, `GetPin`, `SetIrq`                                                                                                  |
| `peripherals.i2c`            | Internal           | `static-server` | `policy-only`                        | `os/i2c`: `ClaimPeripheral`, `SingleTransfer`                                                                                                                     |
| `peripherals.spi`            | Internal           | `static-server` | `policy-only`                        | `os/spi`: `ClaimPeripheral`, `SpiXfer`, `St25r95ReadData`, `NrfReadData`                                                                                          |
| `peripherals.dma`            | Internal           | `static-server` | `policy-only`                        | `os/dma`: `PeripheralTransferMsg`, `ExecuteTransferMsg`, `WaitTransferMsg`, `StopTransferMsg`, `DropTransferMsg`, `FlushTransferMsg`, `SubscribeTransferComplete` |
| `peripherals.power-gating`   | Internal           | `static-server` | `policy-only`                        | `os/power-manager`: `SetPeripheralEnabled`                                                                                                                        |
| `peripherals.feedback`       | Standard           | `signed`    | `install-review`                     | `os/haptics`: `Vibrate`; `os/rgb-server`: `SetAllTo`, `SetTo`, `AnimateAllTo`                                                                                     |

### `settings`

| Subgroup                            | Risk               | Sender policy    | Grant timing         | Messages                                                                                                                                                                                                                                    |
| ----------------------------------- | ------------------ | ---------------- | -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `settings.appearance-display-read`  | Standard           | `signed`    | `install-review`     | `os/settings`: `GetPrimeColor`, `GetSystemTheme`, `SubscribeSystemTheme`, `GetScreenBrightness`, `SubscribeScreenBrightness`                                                                                                                |
| `settings.appearance-display-write` | Standard/Sensitive | `foundation-signed` | `policy-only`        | `os/settings`: `SetSystemTheme`, `SetScreenBrightness`                                                                                                                                                                                      |
| `settings.locale-time-read`         | Standard/Sensitive | `signed`    | `install-review`     | `os/settings`: `LookupTimeZone`, `ListTimeZone`, `GetLocale`, `SubscribeLocale`, `GetUseStandardTimeFormat`, `SubscribeUseStandardTimeFormat`, `GetTimeZone`, `SubscribeTimeZone`                                                           |
| `settings.locale-time-write`        | Standard/Sensitive | `foundation-signed` | `policy-only`        | `os/settings`: `SetLocale`, `SetUseStandardTimeFormat`, `SetTimeZone`                                                                                                                                                                       |
| `settings.device-behavior-read`     | Sensitive          | `signed`    | `install-review`     | `os/settings`: `GetDeviceName`, `SubscribeDeviceName`, `GetAutoLock`, `SubscribeAutoLock`, `GetShowSecurityWords`, `SubscribeShowSecurityWords`, `GetTouchOffset`, `SubscribeTouchOffset`                                                   |
| `settings.device-behavior-write`    | Sensitive/High     | `foundation-signed` | `policy-only`        | `os/settings`: `SetDeviceName`, `SetAutoLock`, `SetShowSecurityWords`, `SetTouchOffset`                                                                                                                                                     |
| `settings.onboarding-backup-envoy`  | Sensitive/High     | `foundation-signed` | `policy-only`        | `os/settings`: `GetOnboardingStatus`, `SetOnboardingStatus`, `SubscribeOnboardingStatus`, `GetEnvoyTimeSync`, `SetEnvoyTimeSync`, `SubscribeEnvoyTimeSync`, `GetMagicBackupEnabled`, `SetMagicBackupEnabled`, `SubscribeMagicBackupEnabled` |
| `settings.hardware-toggle-read`     | Sensitive          | `signed`    | `install-review`     | `os/settings`: `GetAirlockMode`, `SubscribeAirlockMode`, `GetNfcEnabled`, `SubscribeNfcEnabled`, `GetBluetoothEnabled`, `SubscribeBluetoothEnabled`, `GetCameraEnabled`, `SubscribeCameraEnabled`, `GetUsbEnabled`, `SubscribeUsbEnabled`   |
| `settings.hardware-toggle-write`    | Sensitive/High     | `foundation-signed` | `policy-only`        | `os/settings`: `SetAirlockMode`, `SetNfcEnabled`, `SetBluetoothEnabled`, `SetCameraEnabled`, `SetUsbEnabled`                                                                                                                                |
| `settings.developer-system`         | High/Internal      | `static-server`   | `policy-only`        | `os/settings`: `FlushAll`, `ResetSettings`, `GetDebugTouch`, `SetDebugTouch`, `SubscribeDebugTouch`, `GetDeveloperMode`, `SetDeveloperMode`, `SubscribeDeveloperMode`                                                                       |

### `power-and-firmware`

| Subgroup                               | Risk               | Sender policy    | Grant timing     | Messages                                                                                                                                                                         |
| -------------------------------------- | ------------------ | ---------------- | ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `power-and-firmware.power-lifecycle`   | Critical/Internal  | `static-server`   | `policy-only`    | `os/power-manager`: `Shutdown`, `Reboot`                                                                                                                                         |
| `power-and-firmware.battery-status`    | Standard/Sensitive | `signed`    | `install-review` | `os/power-manager-ext`: `GetStatus`, `GetExtendedStatus`, `StatusSubscribe`                                                                                                      |
| `power-and-firmware.usb-power-control` | High/Internal      | `foundation-signed` | `policy-only`    | `os/power-manager-ext`: `SetUsbBoost`, `SetOtgPriority`, `ClearChargeFault`                                                                                                      |
| `developer.battery-sim`                | Developer-only     | `developer`      | `policy-only`    | `os/power-manager-ext`: `SetBatteryPercent`                                                                                                                                      |
| `power-and-firmware.local-update`      | Critical/Internal  | `static-server`   | `policy-only`    | `os/update`: `SubscribeUpdateProgress`, `StartUpdate`, `ContinueUpdate`, `FirmwareVersion`, `ApplyDownloadedUpdate`, `GetUpdateApplied`, `ClearUpdateApplied`, `GetUpdateStatus` |

## UX Recommendations

1. Show only primary groups on the initial review page. Example: `Device secrets`, `File system`, `Network sync`, `Bluetooth/NFC/USB`, `Settings`.
2. Put subgroups behind expandable rows. Most users should not see `os/security.GetSeed` unless they tap technical details or the permission is Critical.
3. Never let a group switch silently enable Critical permissions. A group switch may disable all children, but enabling a group should leave Critical children off until individually reviewed.
4. Treat `Internal` messages as policy-denied for dynamically installed signed apps by default, even if a manifest asks for them.
5. Treat `device-secrets.owner-auth` and `device-secrets.master-seed` as non-grantable to dynamically installed signed apps. A dynamic wallet app should ask for "sign this PSBT" or "derive app-specific seed", not direct `GetSeed`.
6. For `file-system`, the user-facing permission should be the location plus operation, not the raw file operation. Example: "Read USB drive" maps to `GetUsbReadAccess` plus the file read/open messages.
7. For first-use hardware grants, the prompt should name the immediate action. Example: "Allow Foo to use the camera while this screen is open?" or "Allow Foo to connect to BLE device Bar?"
8. For stream/event permissions, copy should say whether the app can keep receiving data in the background.
9. For writes that affect device posture, show consequence copy owned by the OS, not by the requesting app.
10. Show `GetAppSeed` as "App-specific seed", not "Master seed". It is a signed-app credential derived exclusively for that app, so it should not be presented as a master seed export or per-use high-value prompt.
11. For `settings`, dynamically installed non-Foundation signed apps may read and subscribe, but all setting mutations should require `foundation-signed` or stronger.
12. Keep sender policy small and deployment-based. Trusted-publisher management, PIN/seed management, firmware, and system storage should be `static-server`, not merely Foundation-signed dynamic.

## Suggested High-value Review List

These should be surfaced above the regular permission list whenever requested:

- Direct device/PIN material: `GetSeed`, `SetSeed`, `SetSeedAndPin`, `GetPin`, `GetSecurityWords`.
- App-scoped seed access: `GetAppSeed`, shown separately from master seed as an app-specific credential available to signed apps.
- Destructive security actions: `Lockout`, `ChangePin`, `RestoreMasterSeed`.
- Keycard and backup shard movement: `PopShard`, `PushShard`, `StoreShardToKeycard`, `LoadShardFromKeycard`, `BackupShard`, `RestoreShard`, `CreateBackup`, `RestoreBackup`. Show keycard read/export separately from keycard write/restore/format.
- System storage and raw block access: `ReadBlocks`, `WriteBlocks`, `FormatEncryptedVolume`, `FormatAirlock`, `FormatUsb`, all `GetSystem*Access`, `GetBoot*Access`, and `GetEncryptedRoot*Access`.
- Screen and input control: `CaptureScreen`, `InjectTouch`, `InjectKey`, `KeyPressed`, `KeyReleased`.
- Firmware and lifecycle: `StartUpdate`, `ApplyDownloadedUpdate`, `StartFirmwareUpdate`, `NotifyFirmwareInstall`, `Shutdown`, `Reboot`.
- Peripheral/bus control: `os/dma`, `os/spi`, `os/i2c`, `os/gpio`, USB device emulation, and `SetPeripheralEnabled`.
- Publisher trust changes: `ImportThirdPartyCertificate`, `RemoveThirdPartyCertificate`.

## Decisions and Remaining Questions

- Decision: dynamically installed signed apps should never be allowed to send `GetSeed`, `SetSeed`, PIN input, or PIN management messages.
- Decision: signed apps may send `GetAppSeed`, because it returns a seed derived exclusively for that app. Treat it as app-specific credential access, not master seed access.
- Decision: replace the old `third-party-grantable` and `third-party-review` categories with one `signed` sender policy. Use Risk and Grant timing to control whether the UI treats it as ordinary review, high-value review, first-use, or per-use.
- Decision: non-Foundation signed apps may read and subscribe to settings, but may not send settings mutation messages.
- Recommendation: `GetUserReadAccess` and other location gates should be persisted separately from raw file operation messages. Raw file messages are mechanics; the user-facing permission is location + operation.
- Recommendation: first-use grants should start as disclosed-but-inactive manifest requests. When the app first attempts the protected operation, a system broker prompts the user and then either adds the message permission or issues a narrower temporary capability.
- Recommendation: `while-active` grants should be enforced by a broker or by new temporary/revocable kernel permissions. The current additive-only model is enough to approve first use, but not enough to expire a camera/NFC/BLE stream when the foreground session ends.
- Current kernel note: message permissions are additive today. `MessagePermissions` supports `add()` and `is_permitted()`, and the exposed syscalls are `AllowMessagesSID` / `AllowMessagesCID`. There is no matching revoke/remove path in the current kernel permission structure, so runtime revocation would require new kernel/nameserver work or killing/restarting the app.
- Recommendation: trusted publisher / third-party certificate management belongs in a separate "Trusted publishers" surface, and the managing messages should be `static-server`. A Foundation-signed dynamic app should not be enough for this class of permission.
