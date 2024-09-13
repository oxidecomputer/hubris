This folder contains the public keys for our support PIV tokens.

`authorized_keys` is from [`oxidecomputer/support-piv-tokens@36abeb82`](https://github.com/oxidecomputer/support-piv-tokens/blob/36abeb82aacb3640be2f6c988e49844356cf65a8/authorized_keys).

It has been edited to remove spaces from the comment field;
see [`rustcrypto/SSH#289`](https://github.com/RustCrypto/SSH/pull/289)

The `authorized_keys` file is also [mirrored in Omicron](https://github.com/oxidecomputer/omicron/blob/main/smf/switch_zone_setup/support_authorized_keys),
from where it is copied into the host OS (for SSH login into the switch zone).
