# reusemic: NVIDIA Real-time RE-USE as a macOS virtual microphone.
#
#   make              build the driver and the app
#   make model        download the model weights (~54 MB) from Hugging Face
#   make test         run unit + driver tests
#   make install      install the driver (sudo) and restart coreaudiod
#   make uninstall    remove the driver (sudo)
#   make dist         universal (arm64 + x86_64) release zip in build/dist/

DRIVER      := build/ReuseMic.driver
DRIVER_BIN  := $(DRIVER)/Contents/MacOS/ReuseMic
HAL_DIR     := /Library/Audio/Plug-Ins/HAL
MODEL_URL   := https://huggingface.co/nvidia/Real-time_RE-USE/resolve/main/model.safetensors
MODEL       := models/Real-time_RE-USE.safetensors
APP         := target/release/reusemic
CFLAGS      := -Wall -Wextra -O2 -arch arm64 -arch x86_64 -mmacosx-version-min=11.0

VERSION     := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
DIST_NAME   := reusemic-$(VERSION)-macos-universal
DIST_DIR    := build/dist/$(DIST_NAME)

.PHONY: all driver app model test install uninstall clean dist

all: driver app

driver: $(DRIVER_BIN)

$(DRIVER_BIN): driver/ReuseMic.c driver/Info.plist
	@mkdir -p $(DRIVER)/Contents/MacOS
	cp driver/Info.plist $(DRIVER)/Contents/Info.plist
	clang $(CFLAGS) -bundle -o $@ driver/ReuseMic.c -framework CoreAudio -framework CoreFoundation
	codesign --force --sign - $(DRIVER)

app:
	cargo build --release

model: $(MODEL)

$(MODEL):
	@mkdir -p models
	curl -fL --progress-bar -o $@.part $(MODEL_URL)
	mv $@.part $@

build/test_driver: driver/test_driver.c
	@mkdir -p build
	clang -Wall -O1 -o $@ $< -framework CoreAudio -framework CoreFoundation

test: $(DRIVER_BIN) build/test_driver
	./build/test_driver $(DRIVER_BIN)
	cargo test --release

install: $(DRIVER_BIN)
	sudo rm -rf $(HAL_DIR)/ReuseMic.driver
	sudo cp -R $(DRIVER) $(HAL_DIR)/
	sudo chown -R root:wheel $(HAL_DIR)/ReuseMic.driver
	sudo killall -9 coreaudiod || true
	@echo "Installed. \"ReuseMic\" should appear as an input device in a few seconds."

uninstall:
	sudo rm -rf $(HAL_DIR)/ReuseMic.driver
	sudo killall -9 coreaudiod || true

clean:
	rm -rf build
	cargo clean

dist: $(DRIVER_BIN)
	cargo build --release --target aarch64-apple-darwin
	cargo build --release --target x86_64-apple-darwin
	rm -rf $(DIST_DIR) build/dist/$(DIST_NAME).zip
	mkdir -p $(DIST_DIR)
	lipo -create -output $(DIST_DIR)/reusemic \
		target/aarch64-apple-darwin/release/reusemic target/x86_64-apple-darwin/release/reusemic
	codesign --force --sign - $(DIST_DIR)/reusemic
	cp -R $(DRIVER) $(DIST_DIR)/
	cp dist/install.sh dist/uninstall.sh README.md LICENSE-MIT LICENSE-APACHE $(DIST_DIR)/
	cd build/dist && ditto -c -k --norsrc --noextattr --noacl --keepParent $(DIST_NAME) $(DIST_NAME).zip
	cd build/dist && shasum -a 256 $(DIST_NAME).zip > $(DIST_NAME).zip.sha256
	@echo "built build/dist/$(DIST_NAME).zip"
