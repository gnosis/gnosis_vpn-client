#!/bin/bash

# Copyright © 2015-2016 Collabora Ltd.
#
# Permission is hereby granted, free of charge, to any person
# obtaining a copy of this software and associated documentation files
# (the "Software"), to deal in the Software without restriction,
# including without limitation the rights to use, copy, modify, merge,
# publish, distribute, sublicense, and/or sell copies of the Software,
# and to permit persons to whom the Software is furnished to do so,
# subject to the following conditions:
#
# The above copyright notice and this permission notice shall be
# included in all copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
# EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
# MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
# NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS
# BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN
# ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN
# CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.

set -euo pipefail
set -x

NULL=

# ci_distro:
# OS distribution in which we are testing
# Typical values: ubuntu, debian; maybe fedora in future
: "${ci_distro:=ubuntu}"

# ci_docker:
# If non-empty, this is the name of a Docker image. ci-install.sh will
# fetch it with "docker pull" and use it as a base for a new Docker image
# named "ci-image" in which we will do our testing.
: "${ci_docker:=}"

# ci_host:
# Either "native", or an Autoconf --host argument to cross-compile
# the package
: "${ci_host:=native}"

# ci_in_docker:
# Used internally by ci-install.sh. If yes, we are inside the Docker image
# (ci_docker is empty in this case).
: "${ci_in_docker:=no}"

# ci_local_packages:
# prefer local packages instead of distribution
: "${ci_local_packages:=yes}"

# ci_suite:
# OS suite (release, branch) in which we are testing.
# Typical values for ci_distro=debian: sid, bullseye
# Typical values for ci_distro=fedora might be 25, rawhide
: "${ci_suite:=bullseye}"

# ci_variant:
# One of debug, reduced, legacy, production
: "${ci_variant:=production}"

echo "ci_distro=$ci_distro ci_docker=$ci_docker ci_in_docker=$ci_in_docker ci_host=$ci_host ci_local_packages=$ci_local_packages ci_suite=$ci_suite ci_variant=$ci_variant $0"

if [ $(id -u) = 0 ]; then
    sudo=
else
    sudo=sudo
fi

if [ -n "$ci_docker" ]; then
    sed \
        -e "s/@ci_distro@/${ci_distro}/" \
        -e "s/@ci_docker@/${ci_docker}/" \
        -e "s/@ci_suite@/${ci_suite}/" \
        < tools/ci-Dockerfile.in > Dockerfile
    exec docker build -t ci-image .
fi

case "$ci_distro" in
    (debian|ubuntu)
        # Don't ask questions, just do it
        sudo="$sudo env DEBIAN_FRONTEND=noninteractive"

        # Debian Docker images use httpredir.debian.org but it seems to be
        # unreliable; use a CDN instead
        $sudo sed -i -e 's/httpredir\.debian\.org/deb.debian.org/g' \
            /etc/apt/sources.list

        case "$ci_host" in
            (i686-w64-mingw32)
                $sudo dpkg --add-architecture i386
                ;;
            (x86_64-w64-mingw32)
                # assume the host or container is x86_64 already
                ;;
        esac

        $sudo apt-get -qq -y update
        packages=()

        case "$ci_host" in
            (i686-w64-mingw32)
                packages=(
                    "${packages[@]}"
                    binutils-mingw-w64-i686
                    g++-mingw-w64-i686
                    wine32 wine
                )
                ;;
            (x86_64-w64-mingw32)
                packages=(
                    "${packages[@]}"
                    binutils-mingw-w64-x86-64
                    g++-mingw-w64-x86-64
                    wine64 wine
                )
                ;;
        esac

        if [ "$ci_host/$ci_variant/$ci_suite" = "native/production/buster" ]; then
            packages=(
                "${packages[@]}"
                qttools5-dev-tools
                qt5-default
            )
        fi

        packages=(
            "${packages[@]}"
            adduser
            autoconf-archive
            automake
            autotools-dev
            ca-certificates
            ccache
            cmake
            debhelper
            dh-autoreconf
            dh-exec
            docbook-xml
            docbook-xsl
            doxygen
            dpkg-dev
            ducktype
            g++
            gcc
            gnome-desktop-testing
            libapparmor-dev
            libaudit-dev
            libcap-ng-dev
            libexpat-dev
            libglib2.0-dev
            libselinux1-dev
            libsystemd-dev
            libx11-dev
            sudo
            valgrind
            wget
            xauth
            xmlto
            xsltproc
            xvfb
            yelp-tools
            zstd
        )

        $sudo apt-get -qq -y --no-install-recommends install "${packages[@]}"

        if [ "$ci_in_docker" = yes ]; then
            # Add the user that we will use to do the build inside the
            # Docker container, and let them use sudo
            adduser --disabled-password --gecos "" user
            echo "user ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers.d/nopasswd
            chmod 0440 /etc/sudoers.d/nopasswd
        fi

        # Make sure we have a messagebus user, even if the dbus package
        # isn't installed
        $sudo adduser --system --quiet --home /nonexistent --no-create-home \
            --disabled-password --group messagebus
        ;;

    (*)
        echo "Don't know how to set up ${ci_distro}" >&2
        exit 1
        ;;
esac

if [ "$ci_local_packages" = yes ]; then
    case "$ci_host" in
        (*-w64-mingw32)
            cpu="${ci_host%%-*}"
            mirror="https://repo.msys2.org/mingw/$cpu"
            dep_prefix=$(pwd)/${ci_host}-prefix
            # clean install dir, if present
            rm -rf ${dep_prefix}
            install -d "${dep_prefix}"
            wget -O files.lst ${mirror}
            sed 's,^<a href=",,g;s,">.*$,,g' files.lst | grep -v "\.db" | grep -v "\.files" | grep ".*zst$" | sort > filenames.lst
            packages=(
                bzip2
                expat
                gcc-libs
                gettext
                glib2
                iconv
                libffi
                libiconv
                libwinpthread-git
                pcre
                pcre2
                zlib
            )
            for pkg in "${packages[@]}" ; do
                filename=$(grep -F "mingw-w64-${cpu}-${pkg}-" filenames.lst | tail -1)
                if [ -z ${filename} ]; then
                    echo "could not find filename for package '${pkg}'"
                    exit 1
                fi
                # Remove previously downloaded file, which can happen
                # when run locally
                if [ -f ${filename} ]; then
                    rm -rf ${filename}
                fi
                wget ${mirror}/${filename}
                tar -C ${dep_prefix} --strip-components=1 -xvf ${filename}
            done

            # limit access rights
            if [ "$ci_in_docker" = yes ]; then
                chown -R user "${dep_prefix}"
            fi
            ;;
    esac
fi

# vim:set sw=4 sts=4 et:
