#!/bin/sh
# One boot of the disk image test (distro/image/boot-test.sh): the system
# records the boot GRUB chose (openshell-ab booted), then does what the test
# plan on the data partition says for this boot, before it powers off:
#
#   <slot> <release> <confirm|fail|update|desktop|none>    one line per boot
#
# desktop waits for the installed session. confirm exercises just the boot
# transaction. fail leaves both the selector and GRUB's counter untouched.
set -eu

cd /opt/openshell-compat
cmdline=" $(cat /proc/cmdline) "
value() { echo "$cmdline" | sed -n "s/.* $1=\\([^ ]*\\) .*/\\1/p"; }
slot=$(value openshell.slot)
release=$(value openshell.release)

mkdir -p /data
. ./installed-storage.sh
device=$(installed_data_device "$(value openshell.data)")
mount -t ext2 "$device" /data
ab=/data/openshell/ab
fixture=/data/openshell/image-test
plan=$fixture/plan
done_list=$fixture/done
test -f "$plan"
touch "$done_list"
step=$(( $(wc -l < "$done_list") + 1 ))
expected=$(sed -n "${step}p" "$plan")
echo "OPENSHELL_IMAGE_BOOT $step $slot $release (expected $expected)"

set -- $expected
if [ "$slot" != "$1" ] || [ "$release" != "$2" ]; then
    echo "OPENSHELL_IMAGE_WRONG_BOOT $step"
elif [ "$3" = confirm ]; then
    # The failed boots leave grubenv untouched in userspace, as a release
    # that never reaches its boot service would. GRUB alone spends the tries.
    ./openshell-ab "$ab" booted "$slot"
    ./openshell-ab "$ab" confirm "$slot"
elif [ "$3" = update ]; then
    drive=${device%5}
    mkdir -p /mnt/active /mnt/inactive
    mount -t ext2 -o ro "${drive}2" /mnt/active
    mount -t ext2 "${drive}3" /mnt/inactive
    active_hash=$(sha256sum /mnt/active/boot.cfg)
    source=$fixture/release
    manifest=$source/MANIFEST
    keys=$fixture/keys
    update() { ./openshell-ab "$ab" update-mounted "$source" "$manifest" 1 "$keys" "$1"; }
    if update /mnt/active; then
        echo 'Update accepted the running slot' >&2
        exit 1
    fi
    mv "$manifest.sig" "$manifest.saved"
    if update /mnt/inactive; then
        echo 'Update accepted an unsigned release' >&2
        exit 1
    fi
    mv "$manifest.saved" "$manifest.sig"
    ./openshell-ab "$ab" status | grep -Fxq 'b: Empty -'
    cp "$source/boot.cfg" "$source/boot.cfg.saved"
    printf '\n# tampered\n' >> "$source/boot.cfg"
    if update /mnt/inactive; then
        echo 'Update accepted a file that did not match its signed manifest' >&2
        exit 1
    fi
    mv "$source/boot.cfg.saved" "$source/boot.cfg"
    ./openshell-ab "$ab" status | grep -Fxq 'b: Empty -'
    update /mnt/inactive
    ./openshell-ab "$ab" status | grep -Fxq 'b: Trial(3) 2026.12.0'
    test "$active_hash" = "$(sha256sum /mnt/active/boot.cfg)"
    umount /mnt/inactive
    umount /mnt/active
    echo OPENSHELL_IMAGE_UPDATE_OK
elif [ "$3" = desktop ]; then
    sh ./installed-session.sh &
    session=$!
    trap 'kill -TERM "$session" 2>/dev/null || true; wait "$session" 2>/dev/null || true' EXIT
    # The installed session asks for the password first (the first boot
    # creates it): answered on the supervisor's socket as the session user,
    # as the Desktop's sign-in page does.
    ./python -I ./unprivileged.py ./node -e '
        const { createConnection } = require("node:net");
        const { readdirSync } = require("node:fs");
        const socket = () => readdirSync("/tmp").filter(name => name.startsWith("openshell-greeter-") && name !== "openshell-greeter-state")
            .map(name => `/tmp/${name}/greeter.sock`)[0];
        const ask = (path, line) => new Promise((resolve, reject) => {
            const connection = createConnection(path);
            let answer = "";
            connection.on("connect", () => connection.write(line + "\n"));
            connection.on("data", chunk => { answer += chunk; });
            connection.on("close", () => resolve(answer.trim()));
            connection.on("error", reject);
        });
        (async () => {
            for (let tries = 0; tries < 900; tries++) {
                const path = socket();
                if (path) {
                    try {
                        const state = await ask(path, "state");
                        const answer = await ask(path, `${state === "create" ? "create" : "login"} image-test-password`);
                        console.log(`OPENSHELL_IMAGE_SIGN_IN ${state} ${answer}`);
                        return;
                    } catch {}
                }
                await new Promise(done => setTimeout(done, 1000));
            }
            console.log("OPENSHELL_IMAGE_SIGN_IN_TIMEOUT");
        })();
    ' &
    count=0
    while ! ./openshell-ab "$ab" status | grep -Fxq "$slot: Good $release (confirmed)"; do
        kill -0 "$session"
        count=$((count + 1))
        if [ "$count" -ge 180 ]; then
            echo 'OPENSHELL_IMAGE_DESKTOP_TIMEOUT'
            tail -n 40 /data/openshell/session-logs/*/*.log
            tail -n 40 /data/openshell/desktop/hostd.log || true
            ls -l /data/openshell/desktop/hostd/docs || true
            ./node -e 'fetch("http://127.0.0.1:7788/", {signal: AbortSignal.timeout(5000)}).then(r => console.log("OPENSHELL_IMAGE_HOSTD_HTTP", r.status)).catch(() => process.exitCode = 1)' ||
                echo 'OPENSHELL_IMAGE_HOSTD_HTTP_UNREACHABLE'
            kill -TERM "$session"
            wait "$session" || true
            exit 1
        fi
        sleep 2
    done
    kill -TERM "$session"
    wait "$session" || true
    trap - EXIT
    grep -q 'confirmed the boot' /data/openshell/session-logs/*/session.log
    grep -q 'signed in' /data/openshell/session-logs/*/session.log
    test "$(stat -c %a /data/openshell/account/password)" = 600
    test -f /data/openshell/desktop/hostd/docs/Welcome.md
    echo OPENSHELL_IMAGE_DESKTOP_OK
fi
status=$(./openshell-ab "$ab" status 2>/dev/null) || status=none
echo "$status" | sed 's/^/OPENSHELL_IMAGE_STATUS /'
echo "$slot $release" >> "$done_list"
sync
umount /data
echo "OPENSHELL_IMAGE_STEP_DONE $step"
