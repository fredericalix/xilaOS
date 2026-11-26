#!/bin/sh
# Makes the certificates and keys the tests of xila-tls use: made-up
# authorities and servers, good for a hundred years, of no use to anyone.
# The files it made are kept in the repository; running it again makes
# new keys, so everything it writes must then be committed together.
#
#   root (P-384)  ->  between (P-256)  ->  leaf: xila.test    (P-256)
#                                      ->  other: other.test  (same key)
#                                      ->  mixed: xila.test, signed with SHA-384
#                 ->  direct: xila.test, signed with SHA-256
#   stranger: xila.test, by an authority nobody keeps          (same key)
#   rsa-root (RSA 2048)  ->  rsa-leaf: xila.test               (RSA 2048)
set -e
cd "$(dirname "$0")"
days=36500
ca="basicConstraints=critical,CA:TRUE
keyUsage=critical,keyCertSign,cRLSign"
server() {
    printf 'basicConstraints=critical,CA:FALSE\nkeyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:%s\n' "$1"
}
key() {
    openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:"$1" -out "$2" 2>/dev/null
}
signed() { # name subject authority digest extensions
    openssl req -new -key "$1-key.pem" -subj "$2" -out request.pem
    echo "$5" > extensions
    openssl x509 -req -in request.pem -CA "$3.pem" -CAkey "$3-key.pem" -set_serial "0x$(openssl rand -hex 8)" -days $days "-$4" -extfile extensions -out "$1.pem" 2>/dev/null
    rm request.pem extensions
}
own() { # name subject digest
    openssl req -new -x509 -key "$1-key.pem" -subj "$2" -days $days "-$3" -out "$1.pem" -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign"
}

key secp384r1 root-key.pem
own root "/O=XilaOS tests/CN=A made-up root" sha384
key prime256v1 between-key.pem
signed between "/O=XilaOS tests/CN=A made-up authority" root sha384 "$ca"
key prime256v1 leaf-key.pem
signed leaf "/CN=xila.test" between sha256 "$(server xila.test)"
cp leaf-key.pem other-key.pem
signed other "/CN=other.test" between sha256 "$(server other.test)"
# the two curves each with the other's digest, which authorities also do
cp leaf-key.pem mixed-key.pem
signed mixed "/CN=xila.test" between sha384 "$(server xila.test)"
cp leaf-key.pem direct-key.pem
signed direct "/CN=xila.test" root sha256 "$(server xila.test)"
key prime256v1 nobody-key.pem
own nobody "/O=XilaOS tests/CN=An authority nobody keeps" sha256
cp leaf-key.pem stranger-key.pem
signed stranger "/CN=xila.test" nobody sha256 "$(server xila.test)"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out rsa-root-key.pem 2>/dev/null
own rsa-root "/O=XilaOS tests/CN=A made-up RSA root" sha256
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out rsa-leaf-key.pem 2>/dev/null
signed rsa-leaf "/CN=xila.test" rsa-root sha256 "$(server xila.test)"

for name in root between leaf other mixed direct stranger rsa-root rsa-leaf; do
    openssl x509 -in $name.pem -outform DER -out $name.der
done
# the leaf's secret number, as the 32 bytes the test server signs with
openssl ec -in leaf-key.pem -text -noout 2>/dev/null | awk '/priv:/{on=1;next} /pub:/{on=0} on' | tr -d ' :\n' | xxd -r -p > leaf-key.raw
# only what the tests and the checks read is kept
rm -f root-key.pem between-key.pem other-key.pem other.pem mixed-key.pem mixed.pem direct-key.pem direct.pem nobody-key.pem nobody.pem stranger-key.pem stranger.pem rsa-root-key.pem ./*.srl
