# Try Melissa in a virtual machine

> **Pre-alpha.** This is an early demo for trying out and giving feedback. Expect bugs and missing pieces. Don't
> put data you care about on it, and don't expose the VM to the internet. Agents act on your behalf through AI
> accounts you log in to: read what they ask before you approve anything.

You download a ready-made disk image, boot it in a virtual machine on your computer, create your account, set up
one AI agent, and then chat with the Queen (Melissa's lead agent) in your browser. She can install apps, for
example Nextcloud.

What you need:

- A Linux computer with hardware virtualisation (KVM) and one of: QEMU, virt-manager (libvirt). x86_64 only.
- 4 GB of RAM for the VM (3 GB works) and about 10 GB free disk space to start with. The disk is 40 GB but it
  only takes up space as it fills.
- An SSH client (to open the web UI).
- For an agent: nothing for OpenCode's free models, or a Claude or Grok subscription.

## 1. Download and check

Download both files from the release page (the link is in the announcement):

```
melissa-headless-<version>-x86_64.qcow2          about 3 GB
melissa-headless-<version>-x86_64.qcow2.sha256
```

Check the download is complete and unchanged:

```
sha256sum -c melissa-headless-<version>-x86_64.qcow2.sha256
```

It must say `OK`. Keep the downloaded file as a clean copy and boot a copy of it (`cp` it to `melissa.qcow2`),
so you can start over at any time.

## 2. Boot it

The image needs UEFI firmware (not legacy BIOS).

### QEMU (command line)

On Fedora the UEFI firmware is in `/usr/share/edk2/ovmf/` (package `edk2-ovmf`); on Debian/Ubuntu it is
`/usr/share/OVMF/OVMF_CODE_4M.fd` and `OVMF_VARS_4M.fd` (package `ovmf`). Adjust the paths below.

```
cp melissa-headless-<version>-x86_64.qcow2 melissa.qcow2
cp /usr/share/edk2/ovmf/OVMF_VARS.fd melissa-vars.fd
qemu-system-x86_64 -enable-kvm -cpu host -smp 4 -m 4096 -machine q35 \
  -drive if=pflash,format=raw,readonly=on,file=/usr/share/edk2/ovmf/OVMF_CODE.fd \
  -drive if=pflash,format=raw,file=melissa-vars.fd \
  -drive file=melissa.qcow2,if=virtio,format=qcow2 \
  -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:2222-:22 \
  -device virtio-vga
```

A window opens with the VM's screen. `hostfwd` makes the VM's SSH reachable on your computer as
`127.0.0.1` port `2222`; nothing else on your network can reach the VM.

### virt-manager

1. File → New Virtual Machine → **Import existing disk image** → Forward.
2. Browse to your copy `melissa.qcow2`. Operating system: **Fedora** (any recent version) → Forward.
3. Memory **4096** MB, CPUs **2** or more → Forward.
4. Name it `melissa`, tick **Customize configuration before install** → Finish.
5. In Overview set **Firmware: UEFI** → Apply → **Begin Installation** (nothing is installed; it just boots).

The network is libvirt's default NAT: your computer can reach the VM, your LAN can't.

### GNOME Boxes

Boxes boots the image (create a box from the file, give it 4 GB of memory), and the first-boot steps and the
terminal chat (`melissa-queen chat`) work in its window. But Boxes gives your computer no way to connect to the
VM over SSH, which the web UI needs. For the web UI, use virt-manager or QEMU.

## 3. First boot: create your account

On the VM's screen Melissa asks for:

- **a user name and password**: your admin account (it can use `sudo`);
- **an SSH public key** (optional): paste the single line from your `~/.ssh/id_ed25519.pub` (make one with
  `ssh-keygen -t ed25519` if you have none). If you skip it, Melissa offers to turn on SSH password login; that's
  fine for a VM only your computer can reach.

Then press Enter, log in at the prompt, and run:

```
melissa-setup
```

It shows what's running, this VM's address and what to do next. k3s (the app platform) needs a minute or two after
the first boot before it is `active`.

## 4. Set up an agent

The agent programs are made by other companies and are not included in the image. Melissa downloads each one with
its maker's official installer and then runs its login. By installing and logging in you accept that company's
terms. Pick one (you can add more later):

```
sudo melissa-setup agent opencode   # free models, no account (prompts go to OpenCode's hosted service)
sudo melissa-setup agent claude     # needs a Claude subscription: open the link it shows, paste the code back
sudo melissa-setup agent grok       # needs a Grok subscription: open the link, enter the code it shows
```

You log in on your own computer's browser; nothing is typed into Melissa except the code. The logins are stored in
the VM only (`/var/lib/melissa/home`, readable by root and the Queen's service).

Run `sudo melissa-setup` to see which agents are installed and logged in. If a login expires (Grok's do after 7
days): `sudo melissa-setup login grok`.

## 5. Open the web UI

The web UI has no login yet, so it only listens inside the VM. You open it through SSH, which does the
authentication. On **your computer** (not in the VM):

```
# QEMU (port forward from step 2):
ssh -p 2222 -L 8080:127.0.0.1:8080 <your user>@127.0.0.1

# virt-manager: use the address `melissa-setup` shows (often 192.168.122.x):
ssh -L 8080:127.0.0.1:8080 <your user>@<vm address>
```

Keep that SSH session open and browse to **http://127.0.0.1:8080**. (If 8080 is taken on your computer, use
`-L 18080:127.0.0.1:8080` and http://127.0.0.1:18080.)

You can also chat without a browser, in the VM's terminal: `melissa-queen chat`.

## 6. Things to ask the Queen

- "What can you do here?"
- "Check this machine with bee_info and tell me how much memory is free."
- "What apps can you install here?"
- "Install Nextcloud and tell me when it is ready to log in."
- "Is everything healthy?"
- "Create a bee called Ada to look after Nextcloud."
- "Uninstall Nextcloud but keep its data."

Anything that deletes data asks you first; approve or deny it in the UI (or `melissa-queen confirm list`,
`melissa-queen confirm approve <id>` in the VM).

## 7. Install and open Nextcloud

1. Ask the Queen: "Install Nextcloud and tell me when it is ready to log in." The first install downloads about
   0.5 GB, so it takes a few minutes (about 5 on a slow line).
2. Get the admin password, in the VM: `sudo melissa-queen app-credentials nextcloud`
3. On your computer, add this line to `/etc/hosts` (needs sudo), so the name points at your SSH tunnel:
   ```
   127.0.0.1 nextcloud.melissa.home.arpa
   ```
4. Open a tunnel to the VM's web port as well (add it to the SSH command from step 5):
   ```
   ssh -p 2222 -L 8080:127.0.0.1:8080 -L 8081:127.0.0.1:80 <your user>@127.0.0.1
   ```
5. Browse to **http://nextcloud.melissa.home.arpa:8081** and log in as `admin` with that password.

Remove the `/etc/hosts` line when you're done.

## Known limitations (pre-alpha)

- No updates: this image does not update itself. New versions come as a new image.
- No login on the web UI; it is only reachable through SSH on purpose. Don't forward it to other machines.
- Plain HTTP for apps; no TLS and no LAN name service yet (hence the `/etc/hosts` line).
- One machine only; the multi-device hive, backups and Tailscale setup are not part of this demo.
- Agent programs install their makers' latest versions, which may behave differently from the versions we tested.
  If an agent fails to start, tell us its version (`sudo melissa-setup`).
- Agents can be tricked by content they read (prompt injection). Melissa limits what they can do (sandbox, network
  allowlist, confirmations) but can't prevent it. Read confirmations before approving.
- GNOME Boxes: no web UI (see above).
- The UI and the agents' behaviour will change a lot between versions.

## Reset or delete

- **Start over:** shut the VM down, delete `melissa.qcow2` (and `melissa-vars.fd`), copy the downloaded image again.
  In virt-manager, delete the VM with "Delete associated storage files" ticked, then import again.
- **Delete everything:** delete the VM and its disk files. Then revoke the agent logins on the vendors' sites if
  you like: the VM held a login token for each agent you set up. Remove the `/etc/hosts` line if you added it.
- **Shut down from inside:** `sudo systemctl poweroff`.

## Feedback

Tell us what was confusing, what broke (with `melissa-setup` output and, if you can,
`sudo journalctl -u melissa-queen -n 100`) and what you'd like Melissa to do. Check the output for anything
private before you share it.
