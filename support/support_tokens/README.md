This folder contains the public keys for our support PIV tokens and online
tech port unlock.

Most of `authorized_keys` (the `oxide-support-*` entries) is from
[`oxidecomputer/support-piv-tokens@36abeb82`](https://github.com/oxidecomputer/support-piv-tokens/blob/36abeb82aacb3640be2f6c988e49844356cf65a8/authorized_keys),
and is [mirrored in Omicron](https://github.com/oxidecomputer/omicron/blob/main/smf/switch_zone_setup/support_authorized_keys)
from where it is copied into the host OS (for SSH login into the switch zone).

The `Tech Port Unlock` public key was extracted from the Online Signing Service via:
```
permslip --url=https://signer-us-west.corp.oxide.computer public-key "Tech Port Unlock Production 1" > techport.key && ssh-keygen -i -m PKCS8 -f techport.key
```
It should *not* be mirrored into Omicron: it will only be used to sign
tech port unlock challenges, never as a direct authentication key.
