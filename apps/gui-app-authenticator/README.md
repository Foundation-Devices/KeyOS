<!--
SPDX-FileCopyrightText: 2024 Foundation Devices, Inc. <hello@foundation.xyz>
SPDX-License-Identifier: GPL-3.0-or-later
-->

# `gui-app-authenticator`

Authenticator 2FA applet for Passport Prime.

## QA Checklist

- [ ] The app should start up without issues.
- [ ] When there are no codes saved, the app should start at the "add" page.
- [ ] When there are codes saved (archived or not), the app should start at the "main" page.
- [ ] Scanning a QR with a valid 2FA code should open the "edit" page.

![Basic 2FA URL](./media/basic_2fa_url.png)

*`otpauth://totp/Example:alice@google.com?secret=JBSWY3DPEHPK3PXP&issuer=Example`*

- [ ] This QR code should have default values:
  - [ ] Label: "Example"
  - [ ] Account: "alice@google.com"
  - [ ] Issuer: "Example"
- [ ] The following configurations should disable the "Save" button and show warnings:
  - [ ] Empty Label
  - [ ] Duplicate Label with an existing code
  - [ ] Empty Account
  - [ ] Account with ":"
  - [ ] Issuer with ":"
- [ ] Pressing "Save" should take you to the main page where the new card is displayed.
- [ ] If the system time is correct, the generated 2FA codes should work on their corresponding websites.
  - [ ] The provided code should have the same value in another 2FA app like Google Authenticator.
- [ ] Reorder:
  - [ ] "Alphabetical" should sort by case-insenstive alphabetical order.
  - [ ] "Date" should sort by date added.
  - [ ] "Custom" should show up and down arrows at the sides, displays codes in the order set by the user.
  - [ ] Attempting to move the top code up or the bottom code down should not result in any issues.
  - [ ] The search bar should be hidden automatically when clicking "Reorder".
- [ ] Swiping downward on the screen should reveal a search bar.
- [ ] Search:
  - [ ] An empty search bar should show all active codes.
  - [ ] Searches should be case-insensitive.
  - [ ] Clicking a code while the keyboard is visible should hide the keyboard and navigate to the code's page.
- [ ] Clicking a code should navigate to the code's page.
- [ ] Edit code:
  - [ ] The "Archive" button should be shown by default.
  - [ ] If any values are changed, the "Archive" button should become a "Save" button.
  - [ ] The same configurations as above should disable the "Save" button.
  - [ ] Changing the Label and navigating back to the code's page should show the new Label at the top.
  - [ ] Archiving the code should navigate you to the main page, and remove the code from the main page.
- [ ] Archive:
  - [ ] Clicking on an archived code should navigate to an edit page where are fields are disabled.
  - [ ] Restoring an archived code should place it at the end of the main page's custom ordering, with its saved color.
  - [ ] Deleting an archived code should allow a new code with the same Label to be added.
  - [ ] Adding a new code while there are archived codes should not affect the behavior of custom reordering. Ensure the new code can be moved up from the bottom of the list in one click.
- [ ] Errors:
  - [ ] Errors related to scanning QR codes should navigate to the edit page, and display a modal.
  - [ ] Scanning a duplicate of an existing code, even if it is archived, should result in a "Code is already in use" error.
  - [ ] Scanning a code with an invalid URL should result in a "Secret is invalid" error.
  - [ ] Scanning a code with a time period other than 30 seconds should result in a "Secret is invalid" error.
  - [ ] Confirming an error and producing another should not cause any issues.
  - [ ] Confirming an error and scanning a QR successfully should not cause any issues.
  - [ ] Confirming an error and navigating back from the QR scanner should not cause any issues..

![Invalid 2FA URL](./media/invalid_2fa_url.png)

*`otpauth://totp/Exa:mple:alice@google.com?secret=JBSWY3DPEHPK3PXP&issuer=Exa:mple`*

![Invalid Period 2FA URL](./media/invalid_period_2fa_url.png)

*`otpauth://totp/ACME%20Co:john.doe@email.com?secret=HXDMVJECJJWSRB3HWIZR4IFUGFTMXBOZ&issuer=ACME%20Co&algorithm=SHA1&digits=6&period=40`*

- [ ] Scanning a different code with a duplicate Label to an existing code should succeed, but show a warning on the edit page.

![Duplicate Label 2FA URL](./media/duplicate_label_2fa_url.png)

*`otpauth://totp/Example:alice@google.com?secret=ABSWY3DPEHPK3PXP&issuer=Example`*

- [ ] Scanning a code with an empty Issuer should succeed, but show a warning on the edit page due to the empty Label.

![Empty Issuer 2FA URL](./media/empty_issuer_2fa_url.png)

*`otpauth://totp/alice@google.com?secret=JBSWY3DPEHPK3PXP`*

- [ ] Experimental:
  - [ ] 8-digit codes should work but aren't considered in the UI design yet.

![8 Digit 2FA URL](./media/8_digit_2fa_url.png)

*`otpauth://totp/ACME%20Co:john.doe@email.com?secret=HXDMVJECJJWSRB3HWIZR4IFUGFTMXBOZ&issuer=ACME%20Co&algorithm=SHA1&digits=8&period=30`*
