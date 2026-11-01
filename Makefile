# reusemic: NVIDIA Real-time RE-USE as a macOS virtual microphone.
#
#   make              build the driver and the app
#   make model        download the model weights (~54 MB) from Hugging Face
#   make test         run unit + driver tests
#   make install      install the driver (sudo) and restart coreaudiod
#   make uninstall    remove the driver (sudo)

DRIVER      := build/ReuseMic.driver
DRIVER_BIN  := $(DRIVER)/Contents/MacOS/ReuseMic
HAL_DIR     := /Library/Audio/Plug-Ins/HAL
MODEL_URL   := https://huggingface.co/nvidia/Real-time_RE-USE/resolve/main/model.safetensors
MODEL       := models/Real-time_RE-USE.safetensors
APP         := target/release/reusemic
CFLAGS      := -Wall -Wextra -O2 -arch arm64 -arch x86_64 -mmacosx-version-min=11.0

.PHONY: all driver app model test install uninstall clean

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
