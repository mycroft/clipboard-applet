.PHONY: build install install-autostart install-systemd enable-systemd \
	uninstall uninstall-autostart uninstall-systemd

build:
	cargo build --release --locked

install: build
	install -Dm755 target/release/clipboard-applet "$(HOME)/.local/bin/clipboard-applet"

install-autostart: install
	install -Dm644 contrib/clipboard-applet.desktop "$(HOME)/.config/autostart/clipboard-applet.desktop"

install-systemd: install
	install -Dm644 contrib/clipboard-applet.service "$(HOME)/.config/systemd/user/clipboard-applet.service"
	systemctl --user daemon-reload

enable-systemd: install-systemd
	systemctl --user enable --now clipboard-applet.service

uninstall-autostart:
	rm -f "$(HOME)/.config/autostart/clipboard-applet.desktop"

uninstall-systemd:
	-systemctl --user disable --now clipboard-applet.service
	rm -f "$(HOME)/.config/systemd/user/clipboard-applet.service"
	-systemctl --user daemon-reload

uninstall: uninstall-autostart uninstall-systemd
	rm -f "$(HOME)/.local/bin/clipboard-applet"
