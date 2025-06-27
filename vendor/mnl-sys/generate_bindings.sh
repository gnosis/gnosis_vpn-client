#!/usr/bin/env bash

# give libmnl.h path as first argument if you want to use something other than the default

set -ue

HEADER_PATH=${1:-'/usr/include/libmnl/libmnl.h'}
BINDING_PATH='src/bindings.rs'

echo "Generating Rust bindings for $HEADER_PATH"
echo "Writing the result to $BINDING_PATH"

bindgen \
    --no-doc-comments \
    --use-core \
    --no-prepend-enum-name \
    --whitelist-function 'mnl_.+' \
    --whitelist-var 'MNL_.+' \
    --blacklist-type '_.+' \
    --blacklist-type 'FILE' \
    --blacklist-type '(__)?(pid|socklen)_t' \
    --blacklist-type '(nlattr|nlmsghdr)' \
    --raw-line 'use libc::{self, c_int, nlattr, nlmsghdr, pid_t, socklen_t, FILE};' \
    --ctypes-prefix 'libc' \
    -o $BINDING_PATH \
    $HEADER_PATH

# Convert all integer MNL_* constants into c_int for better compatibility with where they are used
sed -i 's/\(pub const MNL_.*: \)[iu]32\(.*\)/\1c_int\2/g' $BINDING_PATH

rustfmt $BINDING_PATH
