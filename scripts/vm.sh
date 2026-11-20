#!/bin/sh
# Live-test VM for Headroom (see AGENTS.md: never run Headroom on the host).
#   scripts/vm.sh create   build the disk (Ubuntu 24.04 cloud image + minimal GNOME)
#   scripts/vm.sh start    boot it; ssh -p 2222 dev@127.0.0.1 (password: dev)
# Inside the VM the repo is at /mnt/headroom (read-only). Add accounts from the Accounts window there.
# Binaries are built on the host (same Ubuntu 24.04 / glibc): cargo build --release -j 2.
# A second VM on a newer GNOME: HEADROOM_VM_DIR=~/headroom-vm-51 HEADROOM_VM_RELEASE=stonking scripts/vm.sh create
set -eu

VM=${HEADROOM_VM_DIR:-$HOME/headroom-vm}
REPO=$(cd "$(dirname "$0")/.." && pwd)
RELEASE=${HEADROOM_VM_RELEASE:-noble}
IMAGE_URL=https://cloud-images.ubuntu.com/$RELEASE/current/$RELEASE-server-cloudimg-amd64.img

create() {
    mkdir -p "$VM"
    [ -f "$VM/$RELEASE.img" ] || curl -fL -o "$VM/$RELEASE.img" "$IMAGE_URL"
    [ ! -f "$VM/disk.qcow2" ] || { echo "$VM/disk.qcow2 exists; delete it to recreate" >&2; exit 1; }
    qemu-img create -q -f qcow2 -F qcow2 -b "$VM/$RELEASE.img" "$VM/disk.qcow2" 16G

    key=$(cat "$HOME"/.ssh/id_ed25519.pub 2>/dev/null || true)
    cat > "$VM/user-data" <<EOF
#cloud-config
hostname: headroom-vm
users:
  - name: dev
    uid: 1000
    groups: [sudo]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: false
    plain_text_passwd: dev
    ssh_authorized_keys: ["$key"]
ssh_pwauth: true
package_update: true
mounts:
  - [repo, /mnt/headroom, 9p, "trans=virtio,version=9p2000.L,ro,nofail", "0", "0"]
write_files:
  - path: /etc/gdm3/custom.conf
    content: |
      [daemon]
      AutomaticLoginEnable=true
      AutomaticLogin=dev
  - path: /etc/profile.d/headroom.sh
    content: |
      PATH=/mnt/headroom/target/release:\$PATH
runcmd:
  - DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends -o Dpkg::Options::=--force-confold gnome-shell gdm3 gnome-session gnome-terminal gnome-keyring libsecret-tools libgtk-4-1 libadwaita-1-0 dbus-user-session xwayland fonts-cantarell adwaita-icon-theme linux-image-extra-virtual
  - apt-get clean
  - systemctl set-default graphical.target
  - reboot
EOF
    printf 'instance-id: headroom-vm\nlocal-hostname: headroom-vm\n' > "$VM/meta-data"
    cloud-localds "$VM/seed.iso" "$VM/user-data" "$VM/meta-data"
    echo "Created. Run: scripts/vm.sh start (first boot installs GNOME, then reboots)"
}

start() {
    exec qemu-system-x86_64 -enable-kvm -cpu host -smp 2 -m 2560 -name headroom-vm \
        -drive file="$VM/disk.qcow2",if=virtio \
        -drive file="$VM/seed.iso",if=virtio,format=raw,readonly=on \
        -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2222-:22 \
        -virtfs local,path="$REPO",mount_tag=repo,security_model=none,readonly=on \
        -device VGA,edid=on,xres=1600,yres=800 -display gtk \
        -monitor unix:"$VM/monitor.sock",server,nowait
    # Screenshot from the host: echo 'screendump /tmp/vm.ppm' | socat - UNIX-CONNECT:$VM/monitor.sock
    # Std VGA, not virtio-vga: with the GTK display, virtio-gpu takes its size from the QEMU window
    # (640x480 at boot) and ignores xres/yres. Std VGA needs bochs from linux-image-extra-virtual.
}

case "${1:-}" in
    create) create ;;
    start) start ;;
    *) echo "usage: $0 create|start" >&2; exit 2 ;;
esac
