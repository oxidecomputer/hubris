This folder contains the public keys for our support PIV tokens and online
tech port unlock.

Most of `authorized_keys` is from [`oxidecomputer/support-piv-tokens@36abeb82`](https://github.com/oxidecomputer/support-piv-tokens/blob/36abeb82aacb3640be2f6c988e49844356cf65a8/authorized_keys),
edited to remove spaces from the comment field; see [`rustcrypto/SSH#289`](https://github.com/RustCrypto/SSH/pull/289).

The `Tech Port Unlock` public key was extracted from the Online Signing Service via:
```
permslip --url=https://signer-us-west.corp.oxide.computer public-key "Tech Port Unlock Production 1" > techport.key && ssh-keygen -i -m PKCS8 -f techport.key
```

The `authorized_keys` file is also [mirrored in Omicron](https://github.com/oxidecomputer/omicron/blob/main/smf/switch_zone_setup/support_authorized_keys)
(except for the Tech Port Unlock key), from where it is copied into the host OS (for SSH login into the switch zone).
