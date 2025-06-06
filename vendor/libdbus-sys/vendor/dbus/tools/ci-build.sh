#!/bin/bash

# Copyright © 2015-2016 Collabora Ltd.
# Copyright © 2020 Ralf Habacker <ralf.habacker@freenet.de>
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

##
## initialize support to run cross compiled executables
##
# syntax: init_wine <path1> [<path2> ... [<pathn>]]
# @param  path1..n  pathes for adding to wine executable search path
#
# The function exits the shell script in case of errors
#
init_wine() {
    if ! command -v wineboot >/dev/null; then
        echo "wineboot not found"
        exit 1
    fi

    # run without X11 display to avoid that wineboot shows dialogs
    wineboot -fi

    # add local paths to wine user path
    local addpath="" d="" i
    for i in "$@"; do
        local wb=$(winepath -w "$i")
        addpath="$addpath$d$wb"
        d=";"
    done

    # create registry file from template
    local wineaddpath=$(echo "$addpath" | sed 's,\\,\\\\\\\\,g')
    sed "s,@PATH@,$wineaddpath,g" ../tools/user-path.reg.in > user-path.reg

    # add path to registry
    wine regedit /C user-path.reg

    # check if path(s) has been set and break if not
    local o=$(wine cmd /C "echo %PATH%")
    case "$o" in
        (*z:* | *Z:*)
            # OK
            ;;
        (*)
            echo "Failed to add Unix paths '$*' to path: Wine %PATH% = $o" >&2
            exit 1
            ;;
    esac
}

# ci_buildsys:
# Build system under test: autotools or cmake
: "${ci_buildsys:=autotools}"

# ci_distro:
# OS distribution in which we are testing
# Typical values: ubuntu, debian; maybe fedora in future
: "${ci_distro:=ubuntu}"

# ci_docker:
# If non-empty, this is the name of a Docker image. ci-install.sh will
# fetch it with "docker pull" and use it as a base for a new Docker image
# named "ci-image" in which we will do our testing.
#
# If empty, we test on "bare metal".
# Typical values: ubuntu:xenial, debian:jessie-slim
: "${ci_docker:=}"

# ci_host:
# See ci-install.sh
: "${ci_host:=native}"

# ci_local_packages:
# prefer local packages instead of distribution
# See ci-install.sh
: "${ci_local_packages:=yes}"

# ci_parallel:
# A number of parallel jobs, passed to make -j
: "${ci_parallel:=1}"

# ci_sudo:
# If yes, assume we can get root using sudo; if no, only use current user
: "${ci_sudo:=no}"

# ci_suite:
# OS suite (release, branch) in which we are testing.
# Typical values for ci_distro=debian: sid, jessie
# Typical values for ci_distro=fedora might be 25, rawhide
: "${ci_suite:=xenial}"

# ci_test:
# If yes, run tests; if no, just build
: "${ci_test:=yes}"

# ci_test_fatal:
# If yes, test failures break the build; if no, they are reported but ignored
: "${ci_test_fatal:=yes}"

# ci_variant:
# One of debug, reduced, legacy, production
: "${ci_variant:=production}"

# ci_runtime:
# One of static, shared; used for windows cross builds
: "${ci_runtime:=static}"

echo "ci_buildsys=$ci_buildsys ci_distro=$ci_distro ci_docker=$ci_docker ci_host=$ci_host ci_local_packages=$ci_local_packages ci_parallel=$ci_parallel ci_suite=$ci_suite ci_test=$ci_test ci_test_fatal=$ci_test_fatal ci_variant=$ci_variant ci_runtime=$ci_runtime $0"

if [ -n "$ci_docker" ]; then
    exec docker run \
        --env=ci_buildsys="${ci_buildsys}" \
        --env=ci_docker="" \
        --env=ci_host="${ci_host}" \
        --env=ci_parallel="${ci_parallel}" \
        --env=ci_sudo=yes \
        --env=ci_test="${ci_test}" \
        --env=ci_test_fatal="${ci_test_fatal}" \
        --env=ci_variant="${ci_variant}" \
        --env=ci_runtime="${ci_runtime}" \
        --privileged \
        ci-image \
        tools/ci-build.sh
fi

maybe_fail_tests () {
    if [ "$ci_test_fatal" = yes ]; then
        exit 1
    fi
}

# Generate config.h.in and configure. We do this for both Autotools and
# CMake builds, so that the CMake build can compare config.h.in with its
# own checks.
NOCONFIGURE=1 ./autogen.sh

case "$ci_buildsys" in
    (cmake-dist)
        # clean up directories from possible previous builds
        rm -rf ci-build-dist
        # Do an Autotools `make dist`, then build *that* with CMake,
        # to assert that our official release tarballs will be enough
        # to build with CMake.
        mkdir -p ci-build-dist
        ( cd ci-build-dist; ../configure )
        make -C ci-build-dist dist
        tar --xz -xvf ci-build-dist/dbus-1.*.tar.xz
        cd dbus-1.*/
        ;;
esac

#
# cross compile setup
#
case "$ci_host" in
    (*-w64-mingw32)
        if [ "$ci_local_packages" = yes ]; then
            dep_prefix=$(pwd)/${ci_host}-prefix
        else
            # assume the compiler was configured with a sysroot (e.g. openSUSE)
            sysroot=$("${ci_host}-gcc" --print-sysroot)
            # check if the prefix is a subdir of sysroot (e.g. openSUSE)
            if [ -d "${sysroot}/${ci_host}" ]; then
                dep_prefix="${sysroot}/${ci_host}"
            else
                # fallback: assume the dependency libraries were built with --prefix=/${ci_host}
                dep_prefix="/${ci_host}"
                export PKG_CONFIG_SYSROOT_DIR="${sysroot}"
            fi
        fi

        export PKG_CONFIG_LIBDIR="${dep_prefix}/lib/pkgconfig"
        export PKG_CONFIG_PATH=
        export PKG_CONFIG="pkg-config --define-variable=prefix=${dep_prefix}"
        unset CC
        unset CXX
        export TMPDIR=/tmp
        ;;
esac

srcdir="$(pwd)"
# clean up directories from possible previous builds
rm -rf ci-build-${ci_variant}-${ci_host}
mkdir -p ci-build-${ci_variant}-${ci_host}
cd ci-build-${ci_variant}-${ci_host}

make="make -j${ci_parallel} V=1 VERBOSE=1"

case "$ci_buildsys" in
    (autotools)
        case "$ci_variant" in
            (debug)
                # Full developer/debug build.
                set _ "$@"
                set "$@" --enable-developer --enable-tests
                # Enable optional features that are off by default
                case "$ci_host" in
                    *-w64-mingw32)
                        ;;
                    *)
                        set "$@" --enable-containers
                        set "$@" --enable-user-session
                        set "$@" SANITIZE_CFLAGS="-fsanitize=address -fsanitize=undefined -fPIE -pie"
                        ;;
                esac
                shift
                # The test coverage for OOM-safety is too
                # verbose to be useful on travis-ci.
                export DBUS_TEST_MALLOC_FAILURES=0
                ;;

            (reduced)
                # A smaller configuration than normal, with
                # various features disabled; this emulates
                # an older system or one that does not have
                # all the optional libraries.
                set _ "$@"
                # No LSMs (the production build has both)
                set "$@" --disable-selinux --disable-apparmor
                # No inotify (we will use dnotify)
                set "$@" --disable-inotify
                # No epoll or kqueue (we will use poll)
                set "$@" --disable-epoll --disable-kqueue
                # No special init system support
                set "$@" --disable-launchd --disable-systemd
                # No libaudit or valgrind
                set "$@" --disable-libaudit --without-valgrind
                # Disable optional features, some of which are on by
                # default
                set "$@" --disable-containers
                set "$@" --disable-stats
                set "$@" --disable-user-session
                shift
                ;;

            (legacy)
                # An unrealistically cut-down configuration,
                # to check that it compiles and works.
                set _ "$@"
                # Disable native atomic operations on Unix
                # (armv4, as used as the baseline for Debian
                # armel, is one architecture that really
                # doesn't have them)
                set "$@" dbus_cv_sync_sub_and_fetch=no
		# Disable getrandom syscall
                set "$@" ac_cv_func_getrandom=no
                # No epoll, kqueue or poll (we will fall back
                # to select, even on Unix where we would
                # usually at least have poll)
                set "$@" --disable-epoll --disable-kqueue
                set "$@" CPPFLAGS=-DBROKEN_POLL=1
                # Enable SELinux and AppArmor but not
                # libaudit - that configuration has sometimes
                # failed
                set "$@" --enable-selinux --enable-apparmor
                set "$@" --disable-libaudit --without-valgrind
                # No directory monitoring at all
                set "$@" --disable-inotify --disable-dnotify
                # No special init system support
                set "$@" --disable-launchd --disable-systemd
                # No X11 autolaunching
                set "$@" --disable-x11-autolaunch
                # Re-enable the deprecated pam_console support to make
                # sure it still builds
                set "$@" --with-console-auth-dir=/var/run/console
                # Leave stats, user-session, etc. at default settings
                # to check that the defaults can compile on an old OS
                shift
                ;;

            (*)
                ;;
        esac

        case "$ci_host" in
            (*-w64-mingw32)
                set _ "$@"
                set "$@" --build="$(build-aux/config.guess)"
                set "$@" --host="${ci_host}"
                set "$@" CFLAGS=-${ci_runtime}-libgcc
                set "$@" CXXFLAGS=-${ci_runtime}-libgcc
                # don't run tests yet, Wine needs Xvfb and
                # more msys2 libraries
                ci_test=no
                # don't "make install" system-wide
                ci_sudo=no
                shift
                ;;
        esac

        ../configure \
            --enable-installed-tests \
            --enable-maintainer-mode \
            --enable-modular-tests \
            "$@"

        ${make}
        [ "$ci_test" = no ] || ${make} check || maybe_fail_tests
        cat test/test-suite.log || :
        [ "$ci_test" = no ] || ${make} distcheck || maybe_fail_tests

        ${make} install DESTDIR=$(pwd)/DESTDIR
        ( cd DESTDIR && find . -ls )

        ${make} -C doc dbus-docs.tar.xz
        tar -C $(pwd)/DESTDIR -xf doc/dbus-docs.tar.xz
        ( cd DESTDIR/dbus-docs && find . -ls )

        if [ "$ci_sudo" = yes ] && [ "$ci_test" = yes ]; then
            sudo ${make} install
            sudo env LD_LIBRARY_PATH=/usr/local/lib \
                /usr/local/bin/dbus-uuidgen --ensure
            LD_LIBRARY_PATH=/usr/local/lib ${make} installcheck || \
                maybe_fail_tests
            cat test/test-suite.log || :

            # re-run them with gnome-desktop-testing
            env LD_LIBRARY_PATH=/usr/local/lib \
            gnome-desktop-testing-runner -d /usr/local/share dbus/ || \
                maybe_fail_tests

            # these tests benefit from being re-run as root, and one
            # test needs a finite fd limit to be useful
            sudo env LD_LIBRARY_PATH=/usr/local/lib \
            bash -c 'ulimit -S -n 1024; ulimit -H -n 4096; exec "$@"' bash \
                gnome-desktop-testing-runner -d /usr/local/share \
                dbus/test-dbus-daemon_with_config.test \
                dbus/test-uid-permissions_with_config.test || \
                maybe_fail_tests
        fi
        ;;

    (cmake|cmake-dist)
        cmdwrapper=
        case "$ci_host" in
            (*-w64-mingw32)
                # CFLAGS and CXXFLAGS does do work, checked with cmake 3.15
                export LDFLAGS="-${ci_runtime}-libgcc"
                # enable tests if supported
                if [ "$ci_test" = yes ]; then
                    libgcc_path=
                    if [ "$ci_runtime" = "shared" ]; then
                        libgcc_path=$(dirname "$("${ci_host}-gcc" -print-libgcc-file-name)")
                    fi
                    init_wine "${dep_prefix}/bin" "$(pwd)/bin" ${libgcc_path:+"$libgcc_path"}
                    cmdwrapper="xvfb-run -a"
                fi
                set _ "$@"
                set "$@" -D CMAKE_TOOLCHAIN_FILE="${srcdir}/cmake/${ci_host}.cmake"
                set "$@" -D CMAKE_PREFIX_PATH="${dep_prefix}"
                if [ "$ci_local_packages" = yes ]; then
                    set "$@" -D CMAKE_INCLUDE_PATH="${dep_prefix}/include"
                    set "$@" -D CMAKE_LIBRARY_PATH="${dep_prefix}/lib"
                    set "$@" -D EXPAT_LIBRARY="${dep_prefix}/lib/libexpat.dll.a"
                    set "$@" -D GLIB2_LIBRARIES="${dep_prefix}/lib/libglib-2.0.dll.a ${dep_prefix}/lib/libgobject-2.0.dll.a ${dep_prefix}/lib/libgio-2.0.dll.a"
                fi
                if [ "$ci_test" = yes ]; then
                    set "$@" -D DBUS_USE_WINE=1
                    # test-dbus-daemon needs more time on Windows
                    export DBUS_TEST_TIMEOUT_MULTIPLIER=2
                fi
                shift
                ;;
        esac

        cmake "$@" -DCMAKE_VERBOSE_MAKEFILE=ON -DENABLE_WERROR=ON ..

        ${make}
        # The test coverage for OOM-safety is too verbose to be useful on
        # travis-ci.
        export DBUS_TEST_MALLOC_FAILURES=0
        [ "$ci_test" = no ] || $cmdwrapper ctest -VV --timeout 180 || maybe_fail_tests
        ${make} install DESTDIR=$(pwd)/DESTDIR
        ( cd DESTDIR && find . -ls)
        ;;
esac

# vim:set sw=4 sts=4 et:
