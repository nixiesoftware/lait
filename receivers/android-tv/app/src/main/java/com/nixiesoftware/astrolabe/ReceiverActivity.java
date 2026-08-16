package com.nixiesoftware.astrolabe;

import android.app.Activity;
import android.os.Bundle;
import android.view.KeyEvent;
import android.view.View;
import android.webkit.WebResourceRequest;
import android.webkit.WebResourceResponse;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import androidx.annotation.Nullable;
import androidx.webkit.WebViewAssetLoader;

public final class ReceiverActivity extends Activity {
    private static final String APP_ORIGIN = "https://appassets.androidplatform.net";
    private WebView webView;
    private NativeTransportBridge transportBridge;

    @Override
    protected void onCreate(@Nullable Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        getWindow().getDecorView().setSystemUiVisibility(
                View.SYSTEM_UI_FLAG_FULLSCREEN
                        | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                        | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY
        );

        WebViewAssetLoader assets = new WebViewAssetLoader.Builder()
                .addPathHandler("/assets/", new WebViewAssetLoader.AssetsPathHandler(this))
                .build();

        webView = new WebView(this);
        WebSettings settings = webView.getSettings();
        settings.setJavaScriptEnabled(true);
        settings.setDomStorageEnabled(false);
        settings.setDatabaseEnabled(false);
        settings.setAllowContentAccess(false);
        settings.setAllowFileAccess(false);
        settings.setMixedContentMode(WebSettings.MIXED_CONTENT_NEVER_ALLOW);
        settings.setMediaPlaybackRequiresUserGesture(false);
        webView.addJavascriptInterface(new SecureStoreBridge(this), "AstrolabeSecureStore");
        transportBridge = new NativeTransportBridge(this, webView);
        webView.addJavascriptInterface(transportBridge, "AstrolabeNativeTransport");
        webView.setWebViewClient(new WebViewClient() {
            @Override
            public WebResourceResponse shouldInterceptRequest(
                    WebView view,
                    WebResourceRequest request
            ) {
                return assets.shouldInterceptRequest(request.getUrl());
            }

            @Override
            public boolean shouldOverrideUrlLoading(WebView view, WebResourceRequest request) {
                return !request.getUrl().toString().startsWith(APP_ORIGIN + "/assets/");
            }
        });
        setContentView(webView);
        webView.loadUrl(APP_ORIGIN + "/assets/index.html");
    }

    private void runReceiverAction(String action) {
        webView.evaluateJavascript(
                "globalThis.astrolabeReceiver && globalThis.astrolabeReceiver." + action + "()",
                null
        );
    }

    @Override
    public boolean onKeyDown(int keyCode, KeyEvent event) {
        if (keyCode == KeyEvent.KEYCODE_DPAD_CENTER || keyCode == KeyEvent.KEYCODE_ENTER) {
            runReceiverAction("confirmPairing");
            return true;
        }
        if (keyCode == KeyEvent.KEYCODE_INFO || keyCode == KeyEvent.KEYCODE_MENU) {
            runReceiverAction("toggleDetails");
            return true;
        }
        if (keyCode == KeyEvent.KEYCODE_BACK) {
            webView.evaluateJavascript(
                    "document.getElementById('pairing-panel').hidden ? 'exit' : 'cancel'",
                    result -> {
                        if ("\"cancel\"".equals(result)) runReceiverAction("cancelPairing");
                        else finish();
                    }
            );
            return true;
        }
        return super.onKeyDown(keyCode, event);
    }

    @Override
    protected void onDestroy() {
        if (webView != null) {
            webView.loadUrl("about:blank");
            webView.removeJavascriptInterface("AstrolabeSecureStore");
            webView.removeJavascriptInterface("AstrolabeNativeTransport");
            if (transportBridge != null) {
                transportBridge.close();
                transportBridge = null;
            }
            webView.destroy();
            webView = null;
        }
        super.onDestroy();
    }
}
