.PHONY: help run build package sign clean

APP_NAME := bfcode-gui
LOVE_VERSION := 11.5
BUNDLE_ID := com.bfcode.gui

help: ## Show this help menu
	@echo "Usage: make [target]"
	@echo ""
	@echo "Targets:"
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-10s %s\n", $$1, $$2}'

run: ## Run with Love2D
	love .

build: ## Build Love2D package (.love)
	@mkdir -p build
	zip -9 -r build/$(APP_NAME).love . -x "build/*" -x ".*" -x "Makefile" -x "*.py" -x "generated_images/*" -x "__pycache__/*" -x "README.md"
	@echo "Built: build/$(APP_NAME).love"

package: build ## Build signed macOS .app bundle
	@echo "==> Downloading LÖVE $(LOVE_VERSION) for macOS..."
	@mkdir -p build
	@curl -L -s -o build/love-macos.zip \
		"https://github.com/love2d/love/releases/download/$(LOVE_VERSION)/love-$(LOVE_VERSION)-macos.zip"
	@unzip -q -o build/love-macos.zip -d build/love-macos
	@# Embed game
	@cp build/$(APP_NAME).love "build/love-macos/love.app/Contents/Resources/game.love"
	@# Update plist
	@plutil -replace CFBundleName -string "$(APP_NAME)" "build/love-macos/love.app/Contents/Info.plist"
	@plutil -replace CFBundleIdentifier -string "$(BUNDLE_ID)" "build/love-macos/love.app/Contents/Info.plist"
	@# Replace icon if exists
	@if [ -f assets/icon.icns ]; then \
		cp assets/icon.icns "build/love-macos/love.app/Contents/Resources/OS X AppIcon.icns" 2>/dev/null || \
		cp assets/icon.icns "build/love-macos/love.app/Contents/Resources/Love.icns" 2>/dev/null || true; \
	elif [ -f assets/icon.png ]; then \
		mkdir -p build/icon.iconset; \
		sips -s format png assets/icon.png --out build/source_icon.png > /dev/null 2>&1; \
		for size in 16 32 64 128 256 512; do \
			sips -s format png -z $$size $$size build/source_icon.png --out "build/icon.iconset/icon_$${size}x$${size}.png" > /dev/null 2>&1; \
		done; \
		for size in 16 32 128 256; do \
			double=$$((size * 2)); \
			cp "build/icon.iconset/icon_$${double}x$${double}.png" "build/icon.iconset/icon_$${size}x$${size}@2x.png"; \
		done; \
		iconutil -c icns build/icon.iconset -o build/icon.icns; \
		cp build/icon.icns "build/love-macos/love.app/Contents/Resources/OS X AppIcon.icns" 2>/dev/null || \
		cp build/icon.icns "build/love-macos/love.app/Contents/Resources/Love.icns" 2>/dev/null || true; \
	fi
	@rm -rf "build/$(APP_NAME).app"
	@mv "build/love-macos/love.app" "build/$(APP_NAME).app"
	@# Sign
	@$(MAKE) sign
	@# Package as zip
	@cd build && ditto -c -k --sequesterRsrc --keepParent $(APP_NAME).app $(APP_NAME)-macos.zip
	@echo "Built: build/$(APP_NAME)-macos.zip"
	@# Cleanup
	@rm -rf build/love-macos build/love-macos.zip build/icon.iconset build/source_icon.png build/icon.icns

sign: ## Sign the .app bundle (requires Developer ID certificate)
	@if [ ! -d "build/$(APP_NAME).app" ]; then \
		echo "Error: build/$(APP_NAME).app not found. Run 'make package' first."; exit 1; \
	fi
	@echo "==> Creating entitlements..."
	@/usr/libexec/PlistBuddy -c "Add :com.apple.security.cs.allow-jit bool true" build/entitlements.plist 2>/dev/null || true
	@/usr/libexec/PlistBuddy -c "Add :com.apple.security.cs.allow-unsigned-executable-memory bool true" build/entitlements.plist 2>/dev/null || true
	@/usr/libexec/PlistBuddy -c "Add :com.apple.security.cs.disable-library-validation bool true" build/entitlements.plist 2>/dev/null || true
	@# Detect signing identity
	$(eval IDENTITY := $(shell security find-identity -v -p codesigning | grep "Developer ID Application" | head -1 | awk -F'"' '{print $$2}'))
	@if [ -z "$(IDENTITY)" ]; then \
		echo "Warning: No Developer ID Application certificate found, using ad-hoc signing"; \
		$(eval IDENTITY := -) \
	fi
	@echo "==> Signing with: $(IDENTITY)"
	@# Sign all Mach-O binaries inside frameworks
	@find "build/$(APP_NAME).app/Contents/Frameworks" -type f | while read f; do \
		if file "$$f" | grep -q "Mach-O"; then \
			echo "  Signing: $$f"; \
			codesign --force --options runtime \
				--sign "$(IDENTITY)" \
				--entitlements build/entitlements.plist \
				--timestamp "$$f"; \
		fi; \
	done
	@# Sign framework bundles
	@find "build/$(APP_NAME).app/Contents/Frameworks" -name "*.framework" -maxdepth 1 | while read f; do \
		echo "  Signing framework: $$f"; \
		codesign --force --options runtime \
			--sign "$(IDENTITY)" \
			--entitlements build/entitlements.plist \
			--timestamp "$$f"; \
	done
	@# Sign main executable
	@echo "  Signing: main executable"
	@codesign --force --options runtime \
		--sign "$(IDENTITY)" \
		--entitlements build/entitlements.plist \
		--timestamp "build/$(APP_NAME).app/Contents/MacOS/love"
	@# Sign outer app bundle
	@echo "  Signing: app bundle"
	@codesign --force --options runtime \
		--sign "$(IDENTITY)" \
		--entitlements build/entitlements.plist \
		--timestamp "build/$(APP_NAME).app"
	@echo "==> Verifying signature..."
	@codesign --verify --deep --strict "build/$(APP_NAME).app"
	@echo "==> Signature OK"
	@rm -f build/entitlements.plist

clean: ## Remove build artifacts
	rm -rf build
