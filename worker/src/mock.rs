// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

// Mock types used only by doctests

#[derive(Debug, Default, Clone)]
pub struct Permissions;

impl server::CheckedPermissions for Permissions {
    const NAME: &str = "";
}

pub struct ScalarEvent(pub u32);
server::wrapped_scalar!(ScalarEvent);

#[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
pub struct ScalarSub;

impl server::MessageId for ScalarSub {
    const ID: server::xous::MessageId = 1;
    const SERVER: &str = "worker/doc-examples";
}

impl server::ScalarSubscription for ScalarSub {
    type Error = server::Infallible;
    type Event = ScalarEvent;
}

impl server::MessageAllowed<ScalarSub> for Permissions {}

#[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
pub struct ArchiveEvent(pub String);

#[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
pub struct ArchiveSub;

impl server::MessageId for ArchiveSub {
    const ID: server::xous::MessageId = 2;
    const SERVER: &str = "worker/doc-examples";
}

impl server::ArchiveSubscription for ArchiveSub {
    type Error = server::Infallible;
    type Event = ArchiveEvent;
}

impl server::MessageAllowed<ArchiveSub> for Permissions {}

pub struct ScalarRequest(pub u32);
server::wrapped_scalar!(ScalarRequest);

impl server::MessageId for ScalarRequest {
    const ID: server::xous::MessageId = 3;
    const SERVER: &str = "worker/doc-examples";
}

impl server::BlockingScalar for ScalarRequest {
    type Response = ScalarResponse;
}

pub struct ScalarResponse(pub u32);
server::wrapped_scalar!(ScalarResponse);

impl server::MessageAllowed<ScalarRequest> for Permissions {}

#[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
pub struct ArchiveRequest {
    pub value: u32,
}

impl server::MessageId for ArchiveRequest {
    const ID: server::xous::MessageId = 4;
    const SERVER: &str = "worker/doc-examples";
}

impl server::BlockingArchive for ArchiveRequest {
    type Response = ArchiveResponse;
}

#[derive(rkyv::Archive, rkyv::Deserialize, rkyv::Serialize)]
pub struct ArchiveResponse {
    pub value: u32,
}

impl server::MessageAllowed<ArchiveRequest> for Permissions {}
