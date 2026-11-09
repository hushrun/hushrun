# Test keys

Private keys for tests only. Never trust them anywhere.

`p256-1.pem`, `p256-2.pem` and `p256-3.pem` are ECDSA P-256 keys in PKCS#8 PEM, written by
OpenSSL so that tests also read keys hushrun didn't write:

```bash
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -pkeyopt ec_param_enc:named_curve -out p256-1.pem
```
