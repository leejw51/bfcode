.PHONY: help run build package clean

APP_NAME := bfcode-gui
UNAME := $(shell uname -s)

help: ## Show this help menu
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-10s %s\n", $$1, $$2}'

run: ## Run with Love2D
	love .

build: ## Build Love2D package (.love)
	@mkdir -p build
	zip -9 -r build/bfcode-gui.love . -x "build/*" -x ".*" -x "Makefile" -x "*.py" -x "generated_images/*" -x "__pycache__/*"
	@echo "Built: build/bfcode-gui.love"

package: build ## Build standalone binary (macOS/Linux/Windows)
ifeq ($(UNAME),Darwin)
	@LOVE_APP=$$(dirname $$(dirname $$(which love))); \
	if [ ! -d "$$LOVE_APP/love.app" ]; then \
		LOVE_APP="/Applications"; \
	fi; \
	if [ ! -d "$$LOVE_APP/love.app" ]; then \
		echo "Error: love.app not found. Install Love2D first."; exit 1; \
	fi; \
	echo "Using $$LOVE_APP/love.app"; \
	rm -rf build/$(APP_NAME).app; \
	cp -R "$$LOVE_APP/love.app" build/$(APP_NAME).app; \
	cp build/$(APP_NAME).love build/$(APP_NAME).app/Contents/Resources/; \
	defaults write "$$(pwd)/build/$(APP_NAME).app/Contents/Info" CFBundleName "$(APP_NAME)"; \
	defaults write "$$(pwd)/build/$(APP_NAME).app/Contents/Info" CFBundleIdentifier "com.bfcode.gui"; \
	plutil -convert xml1 "$$(pwd)/build/$(APP_NAME).app/Contents/Info.plist"; \
	echo "Built: build/$(APP_NAME).app"
else ifeq ($(UNAME),Linux)
	@LOVE_BIN=$$(which love); \
	if [ -z "$$LOVE_BIN" ]; then \
		echo "Error: love not found in PATH. Install Love2D first."; exit 1; \
	fi; \
	cat "$$LOVE_BIN" build/$(APP_NAME).love > build/$(APP_NAME); \
	chmod +x build/$(APP_NAME); \
	echo "Built: build/$(APP_NAME)"
else
	@LOVE_EXE=$$(which love.exe 2>/dev/null || echo ""); \
	if [ -z "$$LOVE_EXE" ]; then \
		echo "Error: love.exe not found in PATH. Install Love2D first."; exit 1; \
	fi; \
	LOVE_DIR=$$(dirname "$$LOVE_EXE"); \
	cat "$$LOVE_EXE" build/$(APP_NAME).love > build/$(APP_NAME).exe; \
	cp "$$LOVE_DIR"/*.dll build/ 2>/dev/null || true; \
	echo "Built: build/$(APP_NAME).exe"
endif

clean: ## Remove build artifacts
	rm -rf build
