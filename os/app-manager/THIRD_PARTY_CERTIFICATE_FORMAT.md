<!--
SPDX-FileCopyrightText: 2026 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# Third-party publisher certificate format

## Encoding

A publisher certificate is an RFC 5280 X.509 v3 certificate. The canonical binary encoding is
DER; PEM `CERTIFICATE` armor is accepted as a transport encoding. Keeping X.509 v3 as the
versioned envelope preserves compatibility with existing `.crt` files and provides an ASN.1 TLV
extension sequence without adding a second wrapper.

Imports are limited to 16 KiB before decoding. The certificate must contain:

- a `secp256k1` EC public key;
- `basicConstraints` with `CA:FALSE`;
- `keyUsage` containing `digitalSignature`; and
- `extendedKeyUsage` containing `codeSigning`.

The subject and subject-alternative-name values are publisher-supplied claims. They are not proof
of publisher identity.

## Publisher fingerprint

The canonical publisher fingerprint is:

```text
lowercase_hex(SHA256(compressed_secp256k1_public_key))
```

The hash input is exactly the 33-byte compressed SEC1 public key, including its `02` or `03`
prefix. The full form is 64 lowercase hexadecimal characters. The short display form is the first
four digest bytes, a Unicode ellipsis, and the last four digest bytes:
`12345678…90abcdef`. The short form is only a display aid; matching and signing-key identity use
the full fingerprint or public key.

Firmware and host tools derive both forms through the shared
`utils/publisher-fingerprint` crate, which is the implementation source of truth for this
algorithm.

## Extension policy and v2 attestation home

Future KeyOS publisher metadata lives in the X.509 v3 `TBSCertificate.extensions` field. Each
feature gets a distinct OID and its value is carried in the extension's DER-encoded `extnValue`
octet string. Extension payloads must begin with their own schema version so they can evolve
independently of the certificate envelope.

V1 firmware decodes the required extensions listed above and `subjectAltName`. It skips unknown
**non-critical** extensions, so a certificate carrying a future attestation block still imports as
an unverified, user-allowed publisher. Unknown critical extensions are rejected as required by RFC
5280 because their semantics cannot safely be ignored.

The v2 publisher attestation block must therefore be a non-critical extension in this sequence.
V2 will assign its OID and inner schema; no unregistered private OID is emitted by v1. Older v1
firmware will ignore that block, while v2 firmware can decode and verify it.
