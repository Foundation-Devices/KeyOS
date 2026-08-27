// SPDX-FileCopyrightText: 2025 Foundation Devices, Inc. <hello@foundation.xyz>
// SPDX-License-Identifier: GPL-3.0-or-later

mod command;
mod error;

use command::{
    Command, Extension, ExtensionOutput, GetAssertionRequest, GetAssertionResponse, GetInfoResponse,
    GetInfoResponseOptions, MakeCredentialRequest, MakeCredentialResponse, PublicKeyCredential,
};
pub use command::{PublicKeyCredentialRpEntity, PublicKeyCredentialUserEntity};
use error::{Error, Status};
use gui_server_api::navigation::securitykeys::UserPresenceOptions;

use crate::{
    implementation::{presence_fingerprint, FidoServer, PresencePoll},
    RegisteredKey,
};

impl FidoServer {
    // stupid naming, but copied from the spec
    fn get_user_presence_flag_value(&self) -> bool {
        // If the pinUvAuthToken is in use then set the userPresentFlagValue to the current value of the
        // pinUvAuthToken’s userPresent flag.
        // Otherwise (implying a pinUvAuthToken exists and is not in use, or does not exist), set
        // userPresentFlagValue to false.
        // Note: The pinUvAuthToken may not exist because the pinUvAuthToken feature is not in use or is not
        // supported.
        false
    }

    /*
    fn get_user_verified_flag_value(&self) -> bool {
        // If the pinUvAuthToken is in use then set the userVerifiedFlagValue to the current value of the
        // pinUvAuthToken’s userVerified flag.
        // Otherwise (implying a pinUvAuthToken exists and is not in use, or does not exist), set
        // userVerifiedFlagValue to false.
        // NOTE: The pinUvAuthToken may not exist because the pinUvAuthToken feature is not in use or is not
        // supported.
        false
    }
    */

    fn clear_user_present_flag(&self) {
        // If the pinUvAuthToken is in use then set the pinUvAuthToken’s userPresent flag to false, otherwise
        // do nothing.
    }

    fn clear_user_verified_flag(&self) {
        // f the pinUvAuthToken is in use then set the pinUvAuthToken’s userVerified flag to false, otherwise
        // do nothing.
    }

    fn clear_pin_uv_auth_token_permissions_except_lbw(&self) {
        // f the pinUvAuthToken is in use then clear all of the pinUvAuthToken’s permissions, except for lbw,
        // otherwise do nothing.
    }

    fn _process_cbor(&mut self, _cmd: u8, _data: &[u8]) -> Result<Vec<u8>, Error> {
        // TODO: CTAP2 implementation needs debugging, disable for now to force U2F-only
        log::warn!("CTAP2 disabled, returning InvalidCommand to force U2F fallback");
        return Err(Error::InvalidCommand);

        // Fingerprint used to correlate retrying RP requests to a pending presence prompt.
        #[allow(unreachable_code)]
        let fingerprint = {
            let mut buf = Vec::with_capacity(1 + _data.len());
            buf.push(_cmd);
            buf.extend_from_slice(_data);
            presence_fingerprint(&buf)
        };

        #[allow(unreachable_code)]
        let prime_options = GetInfoResponseOptions::prime();
        match Command::try_from(_cmd)? {
            Command::GetInfo => {
                log::info!("GetInfo called");
                if _data.len() == 0 {
                    Ok(GetInfoResponse::prime(self.aaguid).to_vec_cbor())
                } else {
                    Err(Error::InvalidLength)
                }
            }
            Command::MakeCredential => {
                log::info!("MakeCredential called");
                let mut req = MakeCredentialRequest::from_cbor(_data)?;
                log::trace!("MakeCredentialRequest: {req:02x?}");
                // 6.1.2. authenticatorMakeCredential Algorithm
                // Upon receipt of an authenticatorMakeCredential request, the authenticator performs the
                // following procedure:
                log::trace!("step 1");
                // 1. If authenticator supports either pinUvAuthToken or clientPin features and the platform
                //    sends a zero length pinUvAuthParam:
                // - Request evidence of user interaction in an authenticator-specific way (e.g., flash the
                //   LED light).
                // - If the user declines permission, or the operation times out, then end the operation by
                //   returning CTAP2_ERR_OPERATION_DENIED.
                // - If evidence of user interaction is provided in this step then return either
                //   CTAP2_ERR_PIN_NOT_SET if PIN is not set or CTAP2_ERR_PIN_INVALID if PIN has been set.
                // Note: This is done for backwards compatibility with CTAP2.0 platforms in the case where
                // multiple authenticators are attached to the platform and the platform wants to enforce
                // pinUvAuthToken feature semantics, but the user has to select which authenticator to get the
                // pinUvAuthToken from. CTAP2.1 platforms SHOULD use § 6.9 authenticatorSelection (0x0B).
                log::trace!("step 2");
                // 2. If the pinUvAuthParam parameter is present:
                if let Some(_pin_uv_auth_param) = req.pin_uv_auth_param {
                    // - If the pinUvAuthProtocol parameter’s value is not supported, return
                    //   CTAP1_ERR_INVALID_PARAMETER error.
                    if let Some(_pin_uv_auth_protocol) = req.pin_uv_auth_protocol {
                        // if !pin_uv_auth_protocol.is_supported() {
                        return Err(Error::InvalidParamter);
                        // }
                    } else {
                        // - If the pinUvAuthProtocol parameter is absent, return CTAP2_ERR_MISSING_PARAMETER
                        //   error.
                        return Err(Error::MissingParamter);
                    }
                }
                log::trace!("step 3");
                // 3. Validate pubKeyCredParams with the following steps:
                // - For each element of pubKeyCredParams:
                //   - If the element is missing required members, including members that are mandatory only
                //     for the specific type, then return an error, for example CTAP2_ERR_INVALID_CBOR.
                //   - If the values of any known members have the wrong type then return an error, for
                //     example CTAP2_ERR_CBOR_UNEXPECTED_TYPE.
                //   - If the element specifies an algorithm that is supported by the authenticator, and no
                //     algorithm has yet been chosen by this loop, then let the algorithm specified by the
                //     current element be the chosen algorithm.
                // - If the loop completes and no algorithm was chosen then return
                //   CTAP2_ERR_UNSUPPORTED_ALGORITHM.
                // Note: This loop chooses the first occurrence of an algorithm identifier supported by this
                // authenticator but always iterates over every element of pubKeyCredParams to validate them.
                if req
                    .pub_key_cred_params
                    .iter()
                    .find(|param| param.alg == PublicKeyCredential::Es256)
                    .is_none()
                {
                    return Err(Error::UnsupportedAlgorithm);
                }
                log::trace!("step 4");
                // 4. Create a new authenticatorMakeCredential response structure and initialize both its "uv"
                //    bit and "up" bit as false.
                let mut resp = MakeCredentialResponse::new(&req.rp.id);
                log::trace!("step 5");
                // 5. If the options parameter is present, process all option keys and values present in the
                //    parameter. Treat any option keys that are not understood as absent.
                // Note: As this specification defines normative behaviours for the "rk", "up", and "uv"
                // option keys, they MUST be understood by all authenticators.
                let mut opt_uv = false;
                let mut opt_up = false;
                let mut opt_rk = false;
                if let Some(opt) = &mut req.options {
                    // - If the "uv" option is absent, let the "uv" option be treated as being present with
                    //   the value false. (This is the default)
                    if opt.uv.is_none() {
                        opt_uv = false;
                    }
                    // - If the pinUvAuthParam is present, let the "uv" option be treated as being present
                    //   with the value false.
                    // Note: pinUvAuthParam and the "uv" option are processed as mutually exclusive with
                    // pinUvAuthParam taking precedence.
                    if req.pin_uv_auth_param.is_some() {
                        opt_uv = false;
                    }
                    // - If the "uv" option is true then:
                    if opt.uv == Some(true) {
                        // - If the authenticator does not support a built-in user verification method end the
                        //   operation by returning CTAP2_ERR_INVALID_OPTION.
                        if prime_options.uv != Some(true) {
                            return Err(Error::InvalidOption);
                        }
                        // - If the built-in user verification method has not yet been enabled, end the
                        //   operation by returning CTAP2_ERR_INVALID_OPTION.
                        opt_uv = true;
                    }
                    // - If the "rk" option is present then:
                    if opt.rk.is_some() {
                        // - If the rk option ID is not present in authenticatorGetInfo response, end the
                        //   operation by returning CTAP2_ERR_UNSUPPORTED_OPTION.
                        return Err(Error::UnsupportedOption);
                    }
                    // - Let the "rk" option be treated as being present with the value false. (This is the
                    //   default.)
                    opt_rk = opt.rk.unwrap_or_default();
                    // - If the "up" option is present then:
                    if opt.up == Some(false) {
                        // - If the "up" option is false, end the operation by returning
                        //   CTAP2_ERR_INVALID_OPTION.
                        return Err(Error::InvalidOption);
                    }
                    // - If the "up" option is absent, let the "up" option be treated as being present with
                    //   the value true (i.e., this is the default for both CTAP2.0 and CTAP2.1
                    //   authenticators).
                    opt_up = opt.up.unwrap_or(true);
                }
                log::trace!("step 6");
                // 6. If the alwaysUv option ID is present and true then:
                if prime_options.always_uv == Some(true) {
                    // - Let the makeCredUvNotRqd option ID be treated as false.
                    // - If the authenticator is not protected by some form of user verification:
                    //   - If the clientPin option ID is present and noMcGaPermissionsWithClientPin option ID
                    //     is absent or false (clientPin is supported for the mc permission):
                    //     - End the operation by returning CTAP2_ERR_PUAT_REQUIRED.
                    //   - Else (clientPin is not supported):
                    //     - End the operation by returning CTAP2_ERR_OPERATION_DENIED.
                    // - If the pinUvAuthParam is not present, and the uv option ID is true, let the "uv"
                    //   option be treated as being present with the value true. Note: The above step 6.3 is
                    //   for backwards compatibility with CTAP2.0 platforms who are
                    // not aware of the Always UV feature.
                    // - If the pinUvAuthParam is not present, and the "uv" option is false or absent:
                    //   - If the clientPin option ID is present and noMcGaPermissionsWithClientPin option ID
                    //     is absent or false (clientPin is supported for the mc permission):
                    //     - End the operation by returning CTAP2_ERR_PUAT_REQUIRED.
                    //   - Else (clientPin is not supported):
                    //     - End the operation by returning CTAP2_ERR_OPERATION_DENIED.
                }
                log::trace!("step 7");
                // 7. If the makeCredUvNotRqd option ID is present and set to true in the authenticatorGetInfo
                //    response:
                // - If the following statements are all true: Note: This step returns an error if the
                //   platform tries to create a discoverable
                // credential without performing some form of user verification.
                //   - The authenticator is protected by some form of user verification.
                //   - The "uv" option is set to false.
                //   - The pinUvAuthParam parameter is not present.
                //   - The "rk" option is present and set to true.
                //   Then:
                //   - If ClientPin option ID is true and the noMcGaPermissionsWithClientPin option ID is
                //     absent or false, end the operation by returning CTAP2_ERR_PUAT_REQUIRED.
                //   - Otherwise, end the operation by returning CTAP2_ERR_OPERATION_DENIED.
                log::trace!("step 8");
                // 8. Else: (the makeCredUvNotRqd option ID in authenticatorGetInfo’s response is present with
                //    the value false or is absent):
                // - If the following statements are all true: Note: This step returns an error if the
                //   platform tries to create a credential without performing some form of user verification
                //   when the makeCredUvNotRqd option ID in authenticatorGetInfo’s response is present with
                //   the value false or is absent.
                //   - The authenticator is protected by some form of user verification.
                //   - The "uv" option is set to false.
                //   - The pinUvAuthParam parameter is not present.
                //   Then:
                //   - If the ClientPin option ID is true and the noMcGaPermissionsWithClientPin option ID is
                //     absent or false, end the operation by returning CTAP2_ERR_PUAT_REQUIRED.
                //   - Otherwise, end the operation by returning CTAP2_ERR_OPERATION_DENIED.
                log::trace!("step 9");
                // 9. If the enterpriseAttestation parameter is present:
                if let Some(_enterprise_attestation) = req.enterprise_attestation {
                    // - If the authenticator is not enterprise attestation capable, or the authenticator is
                    //   enterprise attestation capable but enterprise attestation is disabled, then end the
                    //   operation by returning CTAP1_ERR_INVALID_PARAMETER.
                    return Err(Error::InvalidParamter);
                    // - Else: (the authenticator is enterprise attestation capable and enterprise attestation
                    //   is enabled; see also § 7.1.2 Platform Actions):
                    //   - If the enterpriseAttestation parameter’s value is not 1 or 2, then end the
                    //     operation by returning CTAP2_ERR_INVALID_OPTION.
                    //   - Consider the following cases in order, until one matches, to learn whether the
                    //     authenticator may return an enterprise attestation. (These substeps define when an
                    //     authenticator is permitted to return an enterprise attestation. Authenticators MUST
                    //     NOT do so in any other cases.)
                    //     - If the authenticator supports only vendor-facilitated enterprise attestation and
                    //       the request’s rp.id matches an entry on the authenticator’s pre-configured RP ID
                    //       list, then the authenticator MAY return an enterprise attestation. Note: An
                    //       authenticator that only supports vendor-facilitated enterprise attestation is
                    //       obliged to treat enterpriseAttestation parameter values 1 and 2 equivalently,
                    //       otherwise it will yield unexpected results if used with an enterprise-managed
                    //       platform (which will be setting enterpriseAttestation to 2).
                    //     - If the authenticator supports vendor-facilitated enterprise attestation at all,
                    //       the enterpriseAttestation parameter’s value is 1, and the request’s rp.id matches
                    //       an entry on the authenticator’s pre-configured RP ID list, then the authenticator
                    //       MAY return an enterprise attestation.
                    //     - If the authenticator supports platform-managed enterprise attestation (whether or
                    //       not vendor-facilitated enterprise attestation is also supported), and the
                    //       enterpriseAttestation parameter’s value is 2, then the platform MUST have
                    //       performed the necessary vetting of the request’s rp.id (e.g., via local policy
                    //       lookup), and the authenticator MAY return an enterprise attestation without
                    //       checking whether the request’s rp.id matches an entry on the authenticator’s
                    //       pre-configured RP ID list (if any).
                    //   - If, by considering the substeps of the previous step, the authenticator did not
                    //     conclude that it may return an enterprise attestation then let the
                    //     enterpriseAttestation parameter be treated as absent, terminate these steps, and go
                    //     to Step 10. A non-enterprise attestation will be returned with the credential.
                    //   - Apply any additional constraints that may prohibit returning an enterprise
                    //     attestation. An authenticator has unlimited discretion to apply additional
                    //     constraints which can further limit the contexts in which enterprise attestation is
                    //     returned. They may be based on other parameters from the request or, indeed, on any
                    //     other factor the authenticator wishes. It is the job of enterprise Relying Party to
                    //     know the authenticators that it has deployed and thus to arrange the request so as
                    //     to get its desired result.
                    //   - If, by considering any additional constraints in the previous step, the
                    //     authenticator concluded that it did not wish to return an enterprise attestation
                    //     then let the enterpriseAttestation parameter be treated as absent, terminate these
                    //     steps, and go to Step 10. A non-enterprise attestation will be returned with the
                    //     credential.
                    //   - If the authenticator has a display, then the authenticator SHOULD display an
                    //     explicit warning to the user, including the rp.id, notifying the user that they are
                    //     being uniquely identified to this Relying Party.
                    //   - Let epAtt in the authenticatorMakeCredential response structure be set to true and
                    //     return an enterprise attestation.
                }
                log::trace!("step 10");
                // 10. If the following statements are all true:
                // Note: This step allows the authenticator to create a non-discoverable credential without
                // requiring some form of user verification under the below specific criteria.
                // - "rk" and "uv" options are both set to false or omitted.
                // - the makeCredUvNotRqd option ID in authenticatorGetInfo’s response is present with the
                //   value true.
                // - the pinUvAuthParam parameter is not present.
                // Then go to Step 12.
                // Note: Step 4 has already ensured that the "uv" bit is false in the response.
                let user_interaction_evidence = false;
                if opt_rk || opt_uv || req.pin_uv_auth_param.is_some() {
                    log::trace!("step 11");
                    // 11. If the authenticator is protected by some form of user verification, then:
                    unimplemented!();
                    /*
                      // - If pinUvAuthParam parameter is present (implying the "uv" option is false (see Step
                      //   5)):
                      //   - Call verify(pinUvAuthToken, clientDataHash, pinUvAuthParam).
                      //     - If the verification returns error, then end the operation by returning
                      //       CTAP2_ERR_PIN_AUTH_INVALID error.
                      //   - Verify that the pinUvAuthToken has the mc permission, if not, then end the
                      //     operation by returning CTAP2_ERR_PIN_AUTH_INVALID.
                      //   - If the pinUvAuthToken has a permissions RP ID associated:
                      //     - If the permissions RP ID does not match the rp.id in this request, then end the
                      //       operation by returning CTAP2_ERR_PIN_AUTH_INVALID.
                      //   - Let userVerifiedFlagValue be the result of calling getUserVerifiedFlagValue().
                      //   - If userVerifiedFlagValue is false then end the operation by returning
                      //     CTAP2_ERR_PIN_AUTH_INVALID.
                      //   - If userVerifiedFlagValue is true then set the "uv" bit to true in the response.
                      resp.auth_data.flags.uv = true;
                      //   - If the pinUvAuthToken does not have a permissions RP ID associated:
                      //     - Associate the request’s rp.id parameter value with the pinUvAuthToken as its
                      //       permissions RP ID.
                      //   - Go to Step 12.
                      // - If the "uv" option is present and set to true (implying the pinUvAuthParam parameter
                      //   is not present, and that the authenticator supports an enabled built-in user
                      //   verification method, see Step 5): Note: This step provides backwards compatibility
                      //   for CTAP2.0 platforms.
                      //   - Let internalRetry be true.
                      //   - Let uvState be the result of calling performBuiltInUv(internalRetry)
                      //   - If uvState is error:
                      //     - If the error reason is a user action timeout, then return
                      //       CTAP2_ERR_USER_ACTION_TIMEOUT.
                      //     - If the ClientPin option ID is true and the noMcGaPermissionsWithClientPin option
                      //       ID is absent or false, end the operation by returning CTAP2_ERR_PUAT_REQUIRED.
                      //     - If the uvRetries counter is 0, return CTAP2_ERR_PIN_BLOCKED.
                      //     - Otherwise, end the operation by returning CTAP2_ERR_OPERATION_DENIED.
                      //   - If uvState is success:
                      //     - Set the "uv" bit to true in the response.
                      resp.auth_data.flags.uv = true;
                      //   Note: If Step 11 was skipped, then the authenticator is NOT protected by some form of
                      // user verification, and Step 4 has already ensured that the "uv" bit is false in the
                      // response.
                    */
                }
                log::trace!("step 12");
                let security_key_index = self.security_key_index(None)?;
                // 12. If the excludeList parameter is present and contains a credential ID created by this
                //     authenticator, that is bound to the specified rp.id:
                if let Some(exclude_list) = req.exclude_list {
                    let security_key = self.security_key(security_key_index)?;
                    let registered_key_indexes =
                        security_key.registered_key_indexes(false, req.rp.id.as_bytes());
                    for registered_key_index in registered_key_indexes {
                        let existing_public_key =
                            self.verifying_key_sec1(security_key_index, registered_key_index)?;
                        if exclude_list
                            .iter()
                            .find(|credential| credential.id == existing_public_key)
                            .is_some()
                        {
                            // - If the credential’s credProtect value is not userVerificationRequired, then:
                            // if cred.cred_protect != UserVerificationRequired {
                            if true {
                                // - Let userPresentFlagValue be false.
                                let mut user_present_flag_value = false;
                                // - If the pinUvAuthParam parameter is present then let userPresentFlagValue
                                //   be the result of calling getUserPresentFlagValue().
                                if req.pin_uv_auth_param.is_some() {
                                    user_present_flag_value = self.get_user_presence_flag_value();
                                } else if user_interaction_evidence {
                                    // - Else, if evidence of user interaction was provided as part of Step 11
                                    //   let userPresentFlagValue be true.
                                    user_present_flag_value = true;
                                }
                                // - If userPresentFlagValue is false, then:
                                if !user_present_flag_value {
                                    // Spec requires a presence indication here, but the outcome is
                                    // always CTAP2_ERR_CREDENTIAL_EXCLUDED regardless. Under the
                                    // non-blocking model we skip the advisory prompt and return
                                    // CredentialExcluded directly.
                                    return Err(Error::CredentialExculded);
                                } else {
                                    // - Else, (implying userPresentFlagValue is true) terminate this
                                    //   procedure and return CTAP2_ERR_CREDENTIAL_EXCLUDED.
                                    // Note: A user presence test is required for CTAP2 authenticators, before
                                    // the RP is told that the authenticator is already registered, to behave
                                    // similarly to CTAP1/U2F authenticators.
                                    return Err(Error::CredentialExculded);
                                }
                            } else {
                                // - Else (implying the credential’s credProtect value is
                                //   userVerificationRequired):
                                // - If the "uv" bit is true in the response:
                                if resp.auth_data.flags.uv {
                                    // - Let userPresentFlagValue be false.
                                    let mut user_present_flag_value = false;
                                    // - If the pinUvAuthParam parameter is present then let
                                    //   userPresentFlagValue be the result of calling
                                    //   getUserPresentFlagValue().
                                    if req.pin_uv_auth_param.is_some() {
                                        user_present_flag_value = self.get_user_presence_flag_value();
                                    } else if user_interaction_evidence {
                                        // - Else, if evidence of user interaction was provided as part of
                                        //   Step 11 let userPresentFlagValue be true.
                                        user_present_flag_value = true;
                                    }
                                    // - If userPresentFlagValue is false, then:
                                    if !user_present_flag_value {
                                        // Advisory prompt skipped; return CredentialExcluded per spec.
                                        return Err(Error::CredentialExculded);
                                    } else {
                                        // - Else, (implying userPresentFlagValue is true) terminate this
                                        //   procedure and return CTAP2_ERR_CREDENTIAL_EXCLUDED.
                                        return Err(Error::CredentialExculded);
                                    }
                                } else {
                                    // - Else (implying user verification was not collected in Step 11),
                                    //   remove the credential from the excludeList and continue parsing the
                                    //   rest of the list.
                                }
                            }
                        }
                    }
                }
                log::trace!("step 13");
                // 13. If evidence of user interaction was provided as part of Step 11 (i.e., by invoking
                //     performBuiltInUv()):
                // Note: This step’s criteria implies that the "uv" option is present and set to true and the
                // pinUvAuthParam parameter is not present. I.e., the pinUvAuthToken feature is not in use.
                // - Set the "up" bit to true in the response.
                // resp.auth_data.flags.up = true;
                // - Go to Step 15
                log::trace!("step 14");
                // 14. If the "up" option is set to true:
                if opt_up {
                    // Decide whether a presence prompt is needed (mirrors the spec's branches on
                    // pinUvAuthParam / cached up flag) and run a single non-blocking check.
                    let needs_prompt = if req.pin_uv_auth_param.is_some() {
                        !self.get_user_presence_flag_value()
                    } else {
                        !resp.auth_data.flags.up
                    };
                    if needs_prompt {
                        let mut options = UserPresenceOptions::registration(Some(security_key_index))
                            .with_rp_id(req.rp.id.clone());
                        if let Some(rp_name) = &req.rp.name {
                            options = options.with_rp_name(rp_name.clone());
                        }
                        if let Some(user_name) = &req.user.name {
                            options = options.with_user_name(user_name.clone());
                        }
                        if let Some(user_display_name) = &req.user.display_name {
                            options = options.with_user_display_name(user_display_name.clone());
                        }
                        match self.poll_or_start_presence(fingerprint, options) {
                            PresencePoll::Pending => return Err(Error::UserActionPending),
                            PresencePoll::Dismissed => return Err(Error::OperationDenied),
                            PresencePoll::Confirmed { .. } => {}
                        }
                    }
                    // - Set the "up" bit to true in the response.
                    resp.auth_data.flags.up = true;
                    // - Call clearUserPresentFlag(), clearUserVerifiedFlag(), and
                    //   clearPinUvAuthTokenPermissionsExceptLbw().
                    // Note: This consumes both the "user present state", sometimes referred to as the "cached
                    // UP", and the "user verified state", sometimes referred to as "cached UV". These
                    // functions are no-ops if there is not an in-use pinUvAuthToken.
                    self.clear_user_present_flag();
                    self.clear_user_verified_flag();
                    self.clear_pin_uv_auth_token_permissions_except_lbw();
                }
                log::trace!("step 15");
                // 15. If the extensions parameter is present:
                if let Some(extensions) = &req.extensions {
                    // - Process any extensions that this authenticator supports, ignoring any that it does
                    //   not support.
                    for ext in extensions {
                        // - Authenticator extension outputs generated by the authenticator extension
                        //   processing are returned in the authenticator data. The set of keys in the
                        //   authenticator extension outputs map MUST be equal to, or a subset of, the keys of
                        //   the authenticator extension inputs map.
                        // Note: Some extensions may produce different output depending on the state of the
                        // "uv" bit and/or "up" bit in the response.
                        match ext {
                            Extension::Unknown(s) => {
                                // example of contstructing an extension output
                                let ext_output = ExtensionOutput {
                                    ext: Extension::Unknown(s.to_string()),
                                    data: Vec::new(),
                                };
                                // depending on the extension the signature will cover it or not
                                resp.add_unsigned_extention_output_data(ext_output.clone());
                                resp.auth_data.add_extention_output(ext_output);
                            }
                            _ => (),
                        }
                    }
                }
                log::trace!("step 16");
                // 16. Generate a new credential key pair for the algorithm chosen in step 3.
                let security_key_index = self.security_key_index(None)?;
                // Peek the next credential public key without committing: fallible attestation
                // (step 19) has to run before we mutate registered-key state, or a failure there
                // would persist a half-baked registration the moment the guard drops.
                let public_key = self.peek_next_registration_public_key(security_key_index)?;
                log::trace!("step 17");
                // 17. If the "rk" option is set to true:
                if opt_rk {
                    unimplemented!("rk");
                    // - The authenticator MUST create a discoverable credential.
                    // - If a credential for the same rp.id and account ID already exists on the
                    //   authenticator:
                    //   - If the existing credential contains a largeBlobKey, an authenticator MAY erase any
                    //     associated large-blob data. Platforms MUST NOT assume that authenticators will do
                    //     this. Platforms can later garbage collect any orphaned large-blobs.
                    //   - Overwrite that credential.
                    // - Store the user parameter along with the newly-created key pair.
                    // - If authenticator does not have enough internal storage to persist the new credential,
                    //   return CTAP2_ERR_KEY_STORE_FULL.
                }
                log::trace!("step 18");
                // 18. Otherwise, if the "rk" option is false: the authenticator MUST create a
                //     non-discoverable credential.
                // Note: This step is a change from CTAP2.0 where if the "rk" option is false the
                // authenticator could optionally create a discoverable credential.
                log::trace!("step 19");
                // 19. If the authenticator doesn’t support multiple attestation formats or the
                //     attestationFormatsPreference is absent or its value is the empty list, generate an
                //     attestation statement for the newly-created credential using clientDataHash, taking
                //     into account the value of the enterpriseAttestation parameter, if present, as described
                //     above in Step 9.
                match &req.attestation_formats_preference {
                    None => {
                        resp.attest(
                            &req.client_data_hash.0,
                            &public_key,
                            self.aaguid,
                            &self.attestation_certificate,
                            &self.attestation_pubkey,
                        )?;
                    }
                    Some(formats) => {
                        if formats.is_empty() {
                            resp.attest(
                                &req.client_data_hash.0,
                                &public_key,
                                self.aaguid,
                                &self.attestation_certificate,
                                &self.attestation_pubkey,
                            )?;
                        } else if formats.len() == 1 {
                            // If attestationFormatsPreference is present and contains only one entry with the
                            // value "none", omit attestation from the output.
                            if &formats[0] != "none" {
                                resp.attest(
                                    &req.client_data_hash.0,
                                    &public_key,
                                    self.aaguid,
                                    &self.attestation_certificate,
                                    &self.attestation_pubkey,
                                )?;
                            }
                        } else {
                            resp.attest(
                                &req.client_data_hash.0,
                                &public_key,
                                self.aaguid,
                                &self.attestation_certificate,
                                &self.attestation_pubkey,
                            )?;
                        }
                    }
                }
                // If the authenticator supports multiple attestation formats and the
                // attestationFormatsPreference parameter is present, the authenticator MUST choose a
                // supported format whose attestation statement format identifier appears with the lowest
                // index in the supplied array. If no supported format identifier appears on the list, the
                // authenticator may select a format by any other means.

                // Attestation succeeded — commit state.
                self.create_registered_key_ctap(security_key_index, req.rp.clone(), req.user.clone())?;

                log::trace!("Response: {resp:02x?}");
                Ok(resp.to_vec_cbor())
            }
            Command::GetAssertion => {
                log::info!("GetAssertion called");
                let mut req = GetAssertionRequest::from_cbor(_data)?;
                log::trace!("GetAssertionRequest: {req:02x?}");
                // 6.2.2. authenticatorGetAssertion Algorithm
                // Upon receipt of a authenticatorGetAssertion request, the authenticator performs the
                // following procedure:
                log::trace!("step 1");
                // 1. If authenticator supports either pinUvAuthToken or clientPin features and the platform
                //    sends a zero length pinUvAuthParam:
                // - Request evidence of user interaction in an authenticator-specific way (e.g., flash the
                //   LED light).
                // - If the user declines permission, or the operation times out, then end the operation by
                //   returning CTAP2_ERR_OPERATION_DENIED.
                // - If evidence of user interaction is provided in this step then return either
                //   CTAP2_ERR_PIN_NOT_SET if PIN is not set or CTAP2_ERR_PIN_INVALID if PIN has been set.
                // Note: This is done for backwards compatibility with CTAP2.0 platforms in the case where
                // multiple authenticators are attached to the platform and the platform wants to enforce
                // pinUvAuthToken semantics, but the user has to select which authenticator to get the
                // pinUvAuthToken from. CTAP2.1 platforms SHOULD use § 6.9 authenticatorSelection (0x0B).
                log::trace!("step 2");
                // 2. If the pinUvAuthParam parameter is present:
                if let Some(_pin_uv_auth_param) = req.pin_uv_auth_param {
                    // - If the pinUvAuthProtocol parameter’s value is not supported, return
                    //   CTAP1_ERR_INVALID_PARAMETER error.
                    if let Some(_pin_uv_auth_protocol) = req.pin_uv_auth_protocol {
                        // if !pin_uv_auth_protocol.is_supported() {
                        return Err(Error::InvalidParamter);
                        // }
                    } else {
                        // - If the pinUvAuthProtocol parameter is absent, return CTAP2_ERR_MISSING_PARAMETER
                        //   error.
                        return Err(Error::MissingParamter);
                    }
                }
                log::trace!("step 3");
                // 3. Create a new authenticatorGetAssertion response structure and initialize both its "uv"
                //    bit and "up" bit as false.
                let mut resp = GetAssertionResponse::new(&req.rp_id);
                log::trace!("step 4");
                // 4. If the options parameter is present, process all option keys and values present in the
                //    parameter. Treat any option keys that are not understood as absent.
                // Note: As this specification defines normative behaviours for the "rk", "up", and "uv"
                // option keys, they MUST be understood by all authenticators.
                let mut opt_uv = false;
                let mut opt_up = false;
                if let Some(opt) = &mut req.options {
                    // - If the "uv" option is absent, let the "uv" option be treated as being present with
                    //   the value false. (This is the default)
                    if opt.uv.is_none() {
                        opt_uv = false;
                    }
                    // - If the pinUvAuthParam is present, let the "uv" option be treated as being present
                    //   with the value false.
                    // Note: pinUvAuthParam and the "uv" option are processed as mutually exclusive with
                    // pinUvAuthParam taking precedence.
                    if req.pin_uv_auth_param.is_some() {
                        opt_uv = false;
                    }
                    // - If the "uv" option is present and true then:
                    if opt.uv == Some(true) {
                        // - If the authenticator does not support a built-in user verification method end the
                        //   operation by returning CTAP2_ERR_INVALID_OPTION.
                        if prime_options.uv != Some(true) {
                            return Err(Error::InvalidOption);
                        }
                        // - If the built-in user verification method has not yet been enabled, end the
                        //   operation by returning CTAP2_ERR_INVALID_OPTION.
                        opt_uv = true;
                    }
                    // - If the "rk" option is present then:
                    if opt.rk.is_some() {
                        //   - Return CTAP2_ERR_UNSUPPORTED_OPTION.
                        return Err(Error::UnsupportedOption);
                    }
                    // - If the "up" option is not present then:
                    // - Let the "up" option be treated as being present with the value true. (This is the
                    //   default)
                    opt_up = opt.up.unwrap_or(true);
                }
                log::trace!("step 5");
                // 5. If the alwaysUv option ID is present and true and the "up" option is present and true
                //    then:
                if prime_options.always_uv == Some(true) && req.options.is_some() && opt_up {
                    // - If the authenticator is not protected by some form of user verification:
                    if prime_options.uv != Some(true) {
                        // - If the clientPin option ID is present and noMcGaPermissionsWithClientPin option
                        //   ID is absent or false (clientPin is supported for the ga permission):
                        if prime_options.client_pin.is_some()
                            && prime_options.no_mc_ga_permissions_with_client_pin != Some(true)
                        {
                            // - End the operation by returning CTAP2_ERR_PUAT_REQUIRED.
                            return Err(Error::PuatRequired);
                        } else {
                            // - Else (clientPin is not supported):
                            // - End the operation by returning CTAP2_ERR_OPERATION_DENIED.
                            return Err(Error::OperationDenied);
                        }
                    }
                    // - If the pinUvAuthParam is present then go to Step 6.
                    // - If the "uv" option is true then go to Step 6.
                    if req.pin_uv_auth_param.is_none() && !opt_uv {
                        // - If the "uv" option is false and the authenticator supports a built-in user
                        //   verification method, and the user verification method is enabled then:
                        //   - Let the "uv" option be treated as being present with the value true.
                        //   - Go To Step 6.
                        // - If the clientPin option ID is present and noMcGaPermissionsWithClientPin option
                        //   ID is absent or false, then:
                        // Note: This is to address the case of CTAP2.0 platforms not being aware of and
                        // ignoring the alwaysUv option ID.
                        //   - End the operation by returning CTAP2_ERR_PUAT_REQUIRED.
                        // - Else (clientPin is not supported):
                        //   - End the operation by returning CTAP2_ERR_OPERATION_DENIED.
                        return Err(Error::OperationDenied);
                    }
                }
                log::trace!("step 6");
                // 6. If authenticator is protected by some form of user verification, then:
                let user_interaction_evidence = false;
                if prime_options.uv == Some(true) {
                    unimplemented!("uv");
                    /*
                      // - If pinUvAuthParam parameter is present (implying the "uv" option is treated as false,
                      //   see Step 4):
                      if let Some(_pin_uv_auth_param) = req.pin_uv_auth_param {
                          //   - Call verify(pinUvAuthToken, clientDataHash pinUvAuthParam).
                          //     - If the verification returns error, return CTAP2_ERR_PIN_AUTH_INVALID error.
                          //     - If the verification returns success, set the "uv" bit to true in the
                          //       response.
                          resp.auth_data.flags.uv = true;
                          //   - Let userVerifiedFlagValue be the result of calling getUserVerifiedFlagValue().
                          let user_verified_flag_value = self.get_user_verified_flag_value();
                          //   - If userVerifiedFlagValue is false then end the operation by returning
                          //     CTAP2_ERR_PIN_AUTH_INVALID.
                          if !user_verified_flag_value {
                              return Err(Error::PinAuthInvalid);
                          }
                          //   - Verify that the pinUvAuthToken has the ga permission, if not, return
                          //     CTAP2_ERR_PIN_AUTH_INVALID.
                          //   - If the pinUvAuthToken has a permissions RP ID associated:
                          //     - If the permissions RP ID does not match the rpId in this request, return
                          //       CTAP2_ERR_PIN_AUTH_INVALID.
                          //   - If the pinUvAuthToken does not have a permissions RP ID associated:
                          //     - Associate the request’s rpId parameter value with the pinUvAuthToken as its
                          //       permissions RP ID.
                          //   - Go to Step 7.
                      } else
                      // - If the "uv" option is present and set to true (implying the pinUvAuthParam parameter
                      //   is not present, and that the authenticator supports an enabled built-in user
                      //   verification method, see Step 4): Note: This step provides backwards compatibility
                      //   for CTAP2.0 platforms.
                      if opt_uv {
                          //   - Let internalRetry be true.
                          //   - Let uvState be the result of calling performBuiltInUv(internalRetry)
                          //   - If uvState is error:
                          //     - If the error reason is a user action timeout, then return
                          //       CTAP2_ERR_USER_ACTION_TIMEOUT.
                          //     - If the ClientPin option ID is true and the noMcGaPermissionsWithClientPin
                          //       option ID is absent or false, end the operation by returning
                          //       CTAP2_ERR_PUAT_REQUIRED.
                          //     - If the uvRetries counter is 0, return CTAP2_ERR_PIN_BLOCKED.
                          //     - Otherwise, end the operation by returning CTAP2_ERR_OPERATION_DENIED.
                          //   - If uvState is success:
                          user_interaction_evidence = true;
                          //     - Set the "uv" bit to true in the response.
                          resp.auth_data.flags.uv = true;
                      }
                    */
                }
                // Note: If Step 6 was skipped, then the authenticator is NOT protected by some form of user
                // verification, and Step 3 has already ensured that the "uv" bit is false in the response.
                log::trace!("step 7");
                // 7. Locate all credentials that are eligible for retrieval under the specified criteria:
                let mut applicable_creds = Vec::new();
                let security_key_index = self.security_key_index(None)?;
                // - If the allowList parameter is present and is non-empty
                if let Some(allow_list) = &req.allow_list {
                    if !allow_list.is_empty() {
                        // locate all denoted credentials created by this authenticator and bound to the
                        // specified rpId.
                        let security_key = self.security_key(security_key_index)?;
                        let registered_key_indexes =
                            security_key.registered_key_indexes(false, req.rp_id.as_bytes());
                        for registered_key_index in registered_key_indexes {
                            let public_key =
                                self.verifying_key_sec1(security_key_index, registered_key_index)?;
                            if let RegisteredKey::Ctap(registered_key) =
                                security_key.registered_key(registered_key_index)?
                            {
                                for cred in allow_list {
                                    if cred.id == public_key {
                                        applicable_creds.push((
                                            security_key_index,
                                            registered_key_index,
                                            registered_key,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // - If an allowList is not present, locate all discoverable credentials that are created
                    //   by this authenticator and bound to the specified rpId.
                    let security_key = self.security_key(security_key_index)?;
                    let registered_key_indexes =
                        security_key.registered_key_indexes(false, req.rp_id.as_bytes()); // TODO: add discoverability check
                    for registered_key_index in registered_key_indexes {
                        if let RegisteredKey::Ctap(registered_key) =
                            security_key.registered_key(registered_key_index)?
                        {
                            applicable_creds.push((security_key_index, registered_key_index, registered_key));
                        }
                    }
                }
                // - Create an applicable credentials list populated with the located credentials.
                // - Iterate through the applicable credentials list, and if credential protection for a
                //   credential is marked as userVerificationRequired, and the "uv" bit is false in the
                //   response, remove that credential from the applicable credentials list.
                // - Iterate through the applicable credentials list, and if credential protection for a
                //   credential is marked as userVerificationOptionalWithCredentialIDList and there is no
                //   allowList passed by the client and the "uv" bit is false in the response, remove that
                //   credential from the applicable credentials list.
                // - If the applicable credentials list is empty, return CTAP2_ERR_NO_CREDENTIALS.
                if applicable_creds.is_empty() {
                    return Err(Error::NoCredentials);
                }
                // - Let numberOfCredentials be the number of applicable credentials found.
                let mut number_of_credentials = Some(applicable_creds.len());
                log::trace!("step 8");
                // 8. If evidence of user interaction was provided as part of Step 6.2 (i.e., by invoking
                //    performBuiltInUv()):
                // Note: This step’s criteria implies that the "uv" option is present and set to true and the
                // pinUvAuthParam parameter is not present. I.e., the pinUvAuthToken feature is not in use.
                if user_interaction_evidence {
                    // - Set the "up" bit to true in the response.
                    resp.auth_data.flags.up = true;
                    // - Go to Step 10
                } else {
                    log::trace!("step 9");
                    // 9. If the "up" option is set to true or not present:
                    if opt_up {
                        // Decide whether a presence prompt is needed (mirrors the spec's branches
                        // on pinUvAuthParam / cached up flag) and run a single non-blocking check.
                        let needs_prompt = if req.pin_uv_auth_param.is_some() {
                            !self.get_user_presence_flag_value()
                        } else {
                            !resp.auth_data.flags.up
                        };
                        if needs_prompt {
                            let options = UserPresenceOptions::authentication(Some(security_key_index))
                                .with_rp_id(req.rp_id.clone());
                            match self.poll_or_start_presence(fingerprint, options) {
                                PresencePoll::Pending => return Err(Error::UserActionPending),
                                PresencePoll::Dismissed => return Err(Error::OperationDenied),
                                PresencePoll::Confirmed { .. } => {}
                            }
                        }
                        // - Set the "up" bit to true in the response.
                        resp.auth_data.flags.up = true;
                        // - Call clearUserPresentFlag(), clearUserVerifiedFlag(), and
                        //   clearPinUvAuthTokenPermissionsExceptLbw().
                        // Note: This consumes both the "user present state", sometimes referred to as the
                        // "cached UP", and the "user verified state", sometimes referred to as "cached UV".
                        // These functions are no-ops if there is not an in-use pinUvAuthToken.
                        self.clear_user_present_flag();
                        self.clear_user_verified_flag();
                        self.clear_pin_uv_auth_token_permissions_except_lbw();
                    }
                }
                log::trace!("step 10");
                // 10. If the extensions parameter is present:
                if let Some(extensions) = &req.extensions {
                    // - Process any extensions that this authenticator supports, ignoring any that it does
                    //   not support.
                    for ext in extensions {
                        // - Authenticator extension outputs generated by the authenticator extension
                        //   processing are returned in the authenticator data. The set of keys in the
                        //   authenticator extension outputs map MUST be equal to, or a subset of, the keys of
                        //   the authenticator extension inputs map.
                        // Note: Some extensions may produce different output depending on the state of the
                        // "uv" and/or "up" bits set in the response.
                        match ext {
                            Extension::Unknown(s) => {
                                // example of contstructing an extension output
                                let ext_output = ExtensionOutput {
                                    ext: Extension::Unknown(s.to_string()),
                                    data: Vec::new(),
                                };
                                // depending on the extension the signature will cover it or not
                                resp.add_unsigned_extention_output_data(ext_output.clone());
                                resp.auth_data.add_extention_output(ext_output);
                            }
                            _ => (),
                        }
                    }
                }
                log::trace!("step 11");
                // 11. If the allowList parameter is present:
                let selected_credential = if req.allow_list.is_some() {
                    // - Select any credential from the applicable credentials list.
                    let selected = applicable_creds.pop().unwrap();
                    // - Delete the numberOfCredentials member.
                    number_of_credentials = None;
                    // - Go to Step 13.
                    selected
                } else {
                    log::trace!("step 12");
                    // 12. If allowList is not present:
                    // - If numberOfCredentials is one:
                    if number_of_credentials == Some(1) {
                        // - Select that credential.
                        applicable_creds.pop().unwrap()
                    } else {
                        // - If numberOfCredentials is more than one:
                        unimplemented!();
                        /*
                          // - Order the credentials in the applicable credentials list by the time when they
                          //   were created in reverse order. (I.e. the first credential is the most recently
                          //   created.)
                          // - If the authenticator does not have a display, or the authenticator does have a
                          //   display and the "uv" and "up" options are false:
                          //   - Remember the authenticatorGetAssertion parameters.
                          //   - Create a credential counter (credentialCounter) and set it to 1. This counter
                          //     signifies the next credential to be returned by the authenticator, assuming
                          //     zero-based indexing.
                          //   - Start a timer. This is used during authenticatorGetNextAssertion command. This
                          //     step is OPTIONAL if transport is done over NFC.
                          //   - Select the first credential.
                          // - If authenticator has a display and at least one of the "uv" and "up" options is
                          //   true:
                          //   - Display all the credentials in the applicable credentials list to the user,
                          //     using their friendly name along with other stored account information.
                          //   - Also, display the rpId of the requester (specified in the request) and ask the
                          //     user to select a credential.
                          //   - If the user declines to select a credential or takes too long (as determined by
                          //     the authenticator), terminate this procedure and return the
                          //     CTAP2_ERR_OPERATION_DENIED error.
                          //   - Update the response to set the userSelected member to true and to delete the
                          //     numberOfCredentials member.
                          resp.user_selected = Some(true);
                          resp.number_of_credentials = None;
                          //   - Select the credential indicated by the user.
                        */
                    }
                };
                // - Update the response to include the selected credential’s publicKeyCredentialUserEntity
                //   information. User identifiable information (name, DisplayName, icon) inside the
                //   publicKeyCredentialUserEntity MUST NOT be returned if user verification is not done by
                //   the authenticator.
                resp.set_user(selected_credential.2.user.clone());
                let public_key = self.verifying_key_sec1(selected_credential.0, selected_credential.1)?;
                resp.set_credential(&public_key);
                resp.set_number_of_credentials(number_of_credentials);
                log::trace!("step 13");
                // 13. Sign the clientDataHash along with authData with the selected credential, using the
                //     structure specified in [WebAuthn].
                let mut signature_base = req.client_data_hash.0.to_vec();
                signature_base.extend_from_slice(&resp.auth_data.to_vec());
                (resp.signature, resp.auth_data.sign_count) =
                    self.sign_der(selected_credential.0, selected_credential.1, &signature_base)?;
                log::trace!("Response: {resp:02x?}");
                Ok(resp.to_vec_cbor())
            }
            _ => {
                log::warn!("Command {:?} not implemented", _cmd);
                Err(Error::InvalidSubcommand)
            }
        }
    }

    pub fn ctap_process_cbor(&mut self, cmd: u8, data: &[u8]) -> Vec<u8> {
        let res = self._process_cbor(cmd, data);
        // Only notify when the command completed successfully; pending retries and errors
        // don't mutate FIDO state.
        if res.is_ok() {
            if let Err(e) = self.refresh_and_notify() {
                log::error!("Failed to refresh FIDO keys: {:?}", e);
            }
        }
        Status::from(&res).to_vec(res.unwrap_or_default().as_slice())
    }
}
