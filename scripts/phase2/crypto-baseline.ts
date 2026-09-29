import { writeFileSync } from "node:fs";

import nacl from "tweetnacl";
import {
  decrypt,
  deriveSharedKey,
  encrypt,
  exportPublicKey,
} from "./crypto.ts";

const outputPath = process.argv[2];
if (!outputPath) throw new Error("output path is required");

const bytes = (hex: string) => Uint8Array.from(Buffer.from(hex, "hex"));
const hex = (value: Uint8Array | ArrayBuffer) =>
  Buffer.from(value instanceof Uint8Array ? value : new Uint8Array(value)).toString("hex");

const aliceSecret = bytes("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
const bobSecret = bytes("202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f");
const alice = nacl.box.keyPair.fromSecretKey(aliceSecret);
const bob = nacl.box.keyPair.fromSecretKey(bobSecret);
const shared = deriveSharedKey(alice.secretKey, bob.publicKey);

let randomOffset = 0;
nacl.setPRNG((target, length) => {
  for (let index = 0; index < length; index += 1) {
    target[index] = (randomOffset + index) & 0xff;
  }
  randomOffset += length;
});

const plaintext = bytes("0001027f80ff506173656f");
const bundle = encrypt(shared, plaintext.buffer);
const opened = decrypt(shared, bundle);
const result = {
  baseline: "5de45e208690b0efc51c59a585ae9729325a9204",
  tweetnacl: "1.0.3",
  alicePublic: hex(alice.publicKey),
  bobPublic: hex(bob.publicKey),
  alicePublicBase64: exportPublicKey(alice.publicKey),
  bobPublicBase64: exportPublicKey(bob.publicKey),
  shared: hex(shared),
  bundle: hex(bundle),
  opened: hex(opened),
};

writeFileSync(outputPath, `${JSON.stringify(result, null, 2)}\n`);
