#!/usr/bin/env python3
"""Independent DSSE and Ed25519 implementation, to cross-verify hushrun's envelopes (E-1, E-2).

Ed25519 is the reference code of RFC 8032 section 6; DSSE follows the protocol specification,
and a Sigstore bundle (v0.3) wraps an envelope as the bundle specification says.
Standard library only; Python 3.9 or later. Slow, and meant for tests only.

Usage:
  refdsse.py selftest                          RFC 8032 test vectors and the DSSE PAE example
  refdsse.py public --seed HEX                 print ed25519:<base64> and sha256:<keyid>
  refdsse.py sign PAYLOAD_FILE --seed HEX      print an envelope around the payload bytes
             [--bundle]                        ... inside a bundle, with the key's id as its hint
  refdsse.py verify FILE --key ed25519:B64     verify an envelope or a bundle; print the decoded
                                               payload; exit 1 if not
"""

import argparse
import base64
import hashlib
import json
import sys

PAYLOAD_TYPE = "application/vnd.in-toto+json"
BUNDLE_MEDIA_TYPE = "application/vnd.dev.sigstore.bundle.v0.3+json"

# --- RFC 8032 section 6 ----------------------------------------------------------------------

p = 2 ** 255 - 19
q = 2 ** 252 + 27742317777372353535851937790883648493


def modp_inv(x):
    return pow(x, p - 2, p)


d = -121665 * modp_inv(121666) % p
modp_sqrt_m1 = pow(2, (p - 1) // 4, p)


def sha512(s):
    return hashlib.sha512(s).digest()


def sha512_modq(s):
    return int.from_bytes(sha512(s), "little") % q


def point_add(P, Q):
    A, B = (P[1] - P[0]) * (Q[1] - Q[0]) % p, (P[1] + P[0]) * (Q[1] + Q[0]) % p
    C, D = 2 * P[3] * Q[3] * d % p, 2 * P[2] * Q[2] % p
    E, F, G, H = B - A, D - C, D + C, B + A
    return (E * F, G * H, F * G, E * H)


def point_mul(s, P):
    Q = (0, 1, 1, 0)
    while s > 0:
        if s & 1:
            Q = point_add(Q, P)
        P = point_add(P, P)
        s >>= 1
    return Q


def point_equal(P, Q):
    if (P[0] * Q[2] - Q[0] * P[2]) % p != 0:
        return False
    if (P[1] * Q[2] - Q[1] * P[2]) % p != 0:
        return False
    return True


def recover_x(y, sign):
    if y >= p:
        return None
    x2 = (y * y - 1) * modp_inv(d * y * y + 1)
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (p + 3) // 8, p)
    if (x * x - x2) % p != 0:
        x = x * modp_sqrt_m1 % p
    if (x * x - x2) % p != 0:
        return None
    if (x & 1) != sign:
        x = p - x
    return x


g_y = 4 * modp_inv(5) % p
g_x = recover_x(g_y, 0)
G = (g_x, g_y, 1, g_x * g_y % p)


def point_compress(P):
    zinv = modp_inv(P[2])
    x = P[0] * zinv % p
    y = P[1] * zinv % p
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def point_decompress(s):
    if len(s) != 32:
        return None
    y = int.from_bytes(s, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    x = recover_x(y, sign)
    if x is None:
        return None
    return (x, y, 1, x * y % p)


def secret_expand(secret):
    if len(secret) != 32:
        raise ValueError("an Ed25519 seed is 32 bytes")
    h = sha512(secret)
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def secret_to_public(secret):
    a, _ = secret_expand(secret)
    return point_compress(point_mul(a, G))


def ed25519_sign(secret, msg):
    a, prefix = secret_expand(secret)
    A = point_compress(point_mul(a, G))
    r = sha512_modq(prefix + msg)
    Rs = point_compress(point_mul(r, G))
    h = sha512_modq(Rs + A + msg)
    s = (r + h * a) % q
    return Rs + int.to_bytes(s, 32, "little")


def ed25519_verify(public, msg, signature):
    if len(public) != 32 or len(signature) != 64:
        return False
    A = point_decompress(public)
    if not A:
        return False
    Rs = signature[:32]
    R = point_decompress(Rs)
    if not R:
        return False
    s = int.from_bytes(signature[32:], "little")
    if s >= q:
        return False
    h = sha512_modq(Rs + public + msg)
    return point_equal(point_mul(s, G), point_add(R, point_mul(h, A)))


# --- DSSE and hushrun's key formats ------------------------------------------------------------

def pae(payload_type, body):
    t = payload_type.encode("utf-8")
    return b"DSSEv1 %d %s %d %s" % (len(t), t, len(body), body)


def public_key_text(public):
    return "ed25519:" + base64.b64encode(public).decode()


def keyid(public):
    return "sha256:" + hashlib.sha256(public).hexdigest()


def parse_public_key(text):
    if not text.startswith("ed25519:"):
        raise ValueError("expected ed25519:<base64>")
    raw = base64.b64decode(text[len("ed25519:"):], validate=True)
    if len(raw) != 32:
        raise ValueError("a public key is 32 bytes")
    return raw


def sign_envelope(payload, seed):
    signature = ed25519_sign(seed, pae(PAYLOAD_TYPE, payload))
    public = secret_to_public(seed)
    return {
        "payloadType": PAYLOAD_TYPE,
        "payload": base64.b64encode(payload).decode(),
        "signatures": [{"keyid": keyid(public), "sig": base64.b64encode(signature).decode()}],
    }


def bundle(envelope):
    """A bundle around a key-signed envelope, without timestamps, as hushrun writes it."""
    return {
        "mediaType": BUNDLE_MEDIA_TYPE,
        "verificationMaterial": {
            "publicKey": {"hint": envelope["signatures"][0]["keyid"]},
            "tlogEntries": [],
        },
        "dsseEnvelope": envelope,
    }


def unwrap(document):
    """The envelope of a bundle, or the envelope itself; None for a bundle hushrun doesn't read."""
    if "mediaType" not in document:
        return document
    if document["mediaType"] != BUNDLE_MEDIA_TYPE:
        return None
    envelope = document.get("dsseEnvelope")
    if envelope is None or len(envelope.get("signatures", [])) != 1:
        return None
    hint = document["verificationMaterial"].get("publicKey", {}).get("hint")
    if hint is not None and hint != envelope["signatures"][0].get("keyid"):
        return None
    return envelope


def verify_envelope(envelope, public):
    if envelope.get("payloadType") != PAYLOAD_TYPE:
        return None
    payload = base64.b64decode(envelope["payload"], validate=True)
    message = pae(envelope["payloadType"], payload)
    for sig in envelope.get("signatures", []):
        if sig.get("keyid") != keyid(public):
            continue
        if ed25519_verify(public, message, base64.b64decode(sig["sig"], validate=True)):
            return payload
    return None


def selftest():
    vectors = [
        ("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
         "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a", "",
         "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"),
        ("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
         "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c", "72",
         "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"),
    ]
    for seed, public, message, signature in vectors:
        seed, message = bytes.fromhex(seed), bytes.fromhex(message)
        assert secret_to_public(seed).hex() == public, "public key"
        assert ed25519_sign(seed, message).hex() == signature, "signature"
        assert ed25519_verify(bytes.fromhex(public), message, bytes.fromhex(signature)), "verify"
        assert not ed25519_verify(bytes.fromhex(public), message + b"x", bytes.fromhex(signature))
    assert pae("http://example.com/HelloWorld", b"hello world") == \
        b"DSSEv1 29 http://example.com/HelloWorld 11 hello world"
    print("selftest ok")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("selftest")
    p_public = sub.add_parser("public")
    p_public.add_argument("--seed", required=True)
    p_sign = sub.add_parser("sign")
    p_sign.add_argument("payload")
    p_sign.add_argument("--seed", required=True)
    p_sign.add_argument("--bundle", action="store_true")
    p_verify = sub.add_parser("verify")
    p_verify.add_argument("envelope")
    p_verify.add_argument("--key", required=True)
    args = parser.parse_args()

    if args.command == "selftest":
        selftest()
        return 0
    if args.command == "public":
        public = secret_to_public(bytes.fromhex(args.seed))
        print(public_key_text(public))
        print(keyid(public))
        return 0
    if args.command == "sign":
        with open(args.payload, "rb") as f:
            payload = f.read()
        signed = sign_envelope(payload, bytes.fromhex(args.seed))
        if args.bundle:
            signed = bundle(signed)
        print(json.dumps(signed, separators=(",", ":")))
        return 0
    with open(args.envelope, "rb") as f:
        envelope = unwrap(json.load(f))
    payload = None if envelope is None else verify_envelope(envelope, parse_public_key(args.key))
    if payload is None:
        print("signature does not verify", file=sys.stderr)
        return 1
    sys.stdout.write(payload.decode("utf-8") + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
