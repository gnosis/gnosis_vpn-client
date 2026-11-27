#
# Gnosis VPN (HOPR mixnet) client for OpenWrt
#
# Upstream publishes static-musl aarch64 binaries inside its signed .deb, so no
# rebuild is needed: the package unpacks the official release artifact, pins it
# by PKG_HASH, and ships the three binaries xz-compressed (43 MB of binaries plus
# a 6.5 MB geoip database do not fit a stock overlay; the init script expands
# them into tmpfs).
#
include $(TOPDIR)/rules.mk

PKG_NAME:=gnosisvpn
PKG_VERSION:=1.2.0
PKG_RELEASE:=3
PKG_LICENSE:=LGPL-3.0-only
PKG_LICENSE_FILES:=LICENSE
PKG_MAINTAINER:=LegitClub <hello@legitclub.com>

# Upstream only builds static aarch64 Linux binaries, and the shipped binaries
# are statically linked, so this package is aarch64-only by construction.
PKG_ARCH_DEPENDS:=aarch64

PKG_SOURCE:=$(PKG_NAME)_$(PKG_VERSION)_arm64.deb
PKG_SOURCE_URL:=https://github.com/gnosis/gnosis_vpn/releases/download/v$(PKG_VERSION)
PKG_HASH:=b02c24bdcdbaf6e199e5153a8402f0a6a5ee171ec9f0198c372295adb826d9b8

PKG_BUILD_DEPENDS:=xz/host

include $(INCLUDE_DIR)/package.mk

define Package/gnosisvpn
  SECTION:=net
  CATEGORY:=Network
  SUBMENU:=VPN
  TITLE:=Gnosis VPN (HOPR mixnet) client
  URL:=https://vpn.gnosis.io
  DEPENDS:=+kmod-tun +nftables +ip-full +ca-bundle +shadow-su +xz
endef

define Package/gnosisvpn/description
 Gnosis VPN routes traffic through the HOPR mixnet to Gnosis-operated exits.
 Ships the privileged root daemon, the unprivileged worker and the control CLI.

 The client insists on owning the default route and installs a default-drop
 killswitch, so it runs inside its own network namespace and only LAN traffic
 is steered into it: the router's own uplink (and therefore Tailscale) keeps
 working while the LAN exits through the mixnet.
endef

define Package/gnosisvpn/conffiles
/etc/config/gnosisvpn
/etc/gnosisvpn/config.toml
endef

define Build/Prepare
	mkdir -p $(PKG_BUILD_DIR)
	# A .deb is an ar archive, not a tarball: pull data.tar.gz out first.
	cd $(PKG_BUILD_DIR) && ar p $(DL_DIR)/$(PKG_SOURCE) data.tar.gz | $(TAR) xzf - --no-same-owner
endef

define Build/Compile
	$(STAGING_DIR_HOST)/bin/xz -6 -T0 -c $(PKG_BUILD_DIR)/usr/bin/gnosis_vpn-root > $(PKG_BUILD_DIR)/gnosis_vpn-root.xz
	$(STAGING_DIR_HOST)/bin/xz -6 -T0 -c $(PKG_BUILD_DIR)/usr/bin/gnosis_vpn-worker > $(PKG_BUILD_DIR)/gnosis_vpn-worker.xz
	$(STAGING_DIR_HOST)/bin/xz -6 -T0 -c $(PKG_BUILD_DIR)/usr/bin/gnosis_vpn-ctl > $(PKG_BUILD_DIR)/gnosis_vpn-ctl.xz
	$(STAGING_DIR_HOST)/bin/xz -6 -T0 -c $(PKG_BUILD_DIR)/usr/share/gnosisvpn/geoip.mmdb > $(PKG_BUILD_DIR)/geoip.mmdb.xz
endef

define Package/gnosisvpn/install
	$(INSTALL_DIR) $(1)/usr/share/gnosisvpn/payload
	$(INSTALL_DATA) $(PKG_BUILD_DIR)/gnosis_vpn-root.xz $(1)/usr/share/gnosisvpn/payload/
	$(INSTALL_DATA) $(PKG_BUILD_DIR)/gnosis_vpn-worker.xz $(1)/usr/share/gnosisvpn/payload/
	$(INSTALL_DATA) $(PKG_BUILD_DIR)/gnosis_vpn-ctl.xz $(1)/usr/share/gnosisvpn/payload/
	$(INSTALL_DATA) $(PKG_BUILD_DIR)/geoip.mmdb.xz $(1)/usr/share/gnosisvpn/payload/
	$(INSTALL_DATA) $(PKG_BUILD_DIR)/usr/share/gnosisvpn/geoip.NOTICE $(1)/usr/share/gnosisvpn/
	$(INSTALL_DATA) ./files/LICENSE $(1)/usr/share/gnosisvpn/LICENSE
	$(INSTALL_DIR) $(1)/usr/share/gnosisvpn/curvy-zk

	$(INSTALL_DIR) $(1)/usr/bin
	$(INSTALL_BIN) ./files/gnosis-ctl $(1)/usr/bin/
	$(INSTALL_BIN) ./files/gnosis-ns.sh $(1)/usr/bin/
	$(INSTALL_BIN) ./files/gnosis-ns-run.sh $(1)/usr/bin/
	$(INSTALL_BIN) ./files/gnosis-ns-client.sh $(1)/usr/bin/
	$(INSTALL_BIN) ./files/gnosis-ns-watch.sh $(1)/usr/bin/

	$(INSTALL_DIR) $(1)/etc/init.d
	$(INSTALL_BIN) ./files/gnosisvpn.init $(1)/etc/init.d/gnosisvpn
	$(INSTALL_DIR) $(1)/etc/config
	$(INSTALL_CONF) ./files/gnosisvpn.config $(1)/etc/config/gnosisvpn
	$(INSTALL_DIR) $(1)/etc/gnosisvpn
	$(INSTALL_CONF) ./files/config.toml $(1)/etc/gnosisvpn/config.toml
endef

# The worker refuses to run as root, and this image has no adduser/useradd, so
# the account is written straight into the user database.
define Package/gnosisvpn/postinst
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
grep -q '^gnosisvpn:' /etc/group || echo 'gnosisvpn:x:65533:' >> /etc/group
grep -q '^gnosisvpn:' /etc/passwd || echo 'gnosisvpn:x:65533:65533:gnosisvpn:/var/lib/gnosisvpn:/bin/sh' >> /etc/passwd
grep -q '^gnosisvpn:' /etc/shadow || echo 'gnosisvpn:!:19000:0:99999:7:::' >> /etc/shadow
mkdir -p /var/lib/gnosisvpn /run/gnosisvpn /var/log/gnosisvpn
chown -R 65533:65533 /var/lib/gnosisvpn /run/gnosisvpn /var/log/gnosisvpn
/etc/init.d/gnosisvpn enable
exit 0
endef

define Package/gnosisvpn/prerm
#!/bin/sh
[ -n "$${IPKG_INSTROOT}" ] && exit 0
[ -x /usr/bin/gnosis-ns.sh ] && /usr/bin/gnosis-ns.sh down >/dev/null 2>&1
/etc/init.d/gnosisvpn stop
/etc/init.d/gnosisvpn disable
exit 0
endef

$(eval $(call BuildPackage,gnosisvpn))