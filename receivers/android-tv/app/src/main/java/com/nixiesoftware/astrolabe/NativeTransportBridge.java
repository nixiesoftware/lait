package com.nixiesoftware.astrolabe;

import android.content.Context;
import android.util.Base64;
import android.webkit.JavascriptInterface;
import android.webkit.WebView;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.URL;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.security.cert.CertificateException;
import java.security.cert.CertificateFactory;
import java.security.cert.X509Certificate;
import java.util.Arrays;
import java.util.HashSet;
import java.util.Iterator;
import java.util.Locale;
import java.util.Set;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.RejectedExecutionException;

import javax.net.ssl.HttpsURLConnection;
import javax.net.ssl.SSLContext;
import javax.net.ssl.SSLSocketFactory;
import javax.net.ssl.TrustManager;
import javax.net.ssl.X509TrustManager;

/** Request-local HTTPS transport for the bundled receiver surface. */
public final class NativeTransportBridge {
    private static final int MAX_BOOTSTRAP_BYTES = 32 * 1024;
    private static final int MAX_BODY_BYTES = 16 * 1024 * 1024;
    private static final Set<String> REQUEST_FIELDS = fields(
            "method", "url", "body", "headers", "maximum_bytes", "timeout_ms"
    );
    private static final Set<String> ALLOWED_HEADERS = fields(
            "accept", "content-type", "authorization", "range",
            "x-astrolabe-protocol-major", "x-astrolabe-route", "x-astrolabe-device",
            "x-astrolabe-challenge", "x-astrolabe-body-sha256", "x-astrolabe-assignment",
            "x-astrolabe-program", "x-astrolabe-revision", "x-astrolabe-current-item",
            "x-astrolabe-elapsed-ms", "x-astrolabe-wait-ms", "x-astrolabe-asset"
    );

    private final String bootstrapJson;
    private final URL origin;
    private final SSLSocketFactory pinnedFactory;
    private final WebView webView;
    private final ExecutorService requests = Executors.newSingleThreadExecutor();
    private volatile boolean closed;

    NativeTransportBridge(Context context, WebView webView) {
        this.webView = webView;
        try {
            bootstrapJson = readBootstrap(context);
            JSONObject bootstrap = new JSONObject(bootstrapJson);
            requireFields(bootstrap, fields("protocol_major", "trust", "certificate_pem", "rendezvous"));
            if (bootstrap.getInt("protocol_major") != 1) throw new IOException("unsupported protocol major");
            JSONObject trust = bootstrap.getJSONObject("trust");
            String kind = trust.getString("kind");
            if ("web_pki_origin".equals(kind)) {
                requireFields(trust, fields("kind", "origin"));
                if (!bootstrap.isNull("certificate_pem")) throw new IOException("unexpected Web PKI certificate");
                pinnedFactory = null;
            } else if ("pinned_certificate".equals(kind)) {
                requireFields(trust, fields("kind", "origin", "sha256"));
                pinnedFactory = pinnedFactory(
                        bootstrap.getString("certificate_pem"),
                        decodeFingerprint(trust.getString("sha256"))
                );
            } else {
                throw new IOException("unsupported trust kind");
            }
            origin = exactOrigin(trust.getString("origin"));
        } catch (Exception error) {
            throw new IllegalStateException("Invalid Astrolabe receiver bootstrap", error);
        }
    }

    @JavascriptInterface
    public String bootstrap() {
        return bootstrapJson;
    }

    @JavascriptInterface
    public void request(String requestId, String requestJson) {
        if (requestId == null || !requestId.matches("^[0-9]{1,20}$")) return;
        try {
            requests.execute(() -> deliver(requestId, performRequest(requestJson)));
        } catch (RejectedExecutionException error) {
            deliver(requestId, error("native transport is closed"));
        }
    }

    void close() {
        closed = true;
        requests.shutdownNow();
    }

    private String performRequest(String requestJson) {
        HttpsURLConnection connection = null;
        try {
            if (requestJson == null || requestJson.length() > 128 * 1024) {
                throw new IOException("native request envelope exceeds bound");
            }
            JSONObject request = new JSONObject(requestJson);
            requireFields(request, REQUEST_FIELDS);
            String method = request.getString("method");
            if (!"GET".equals(method) && !"POST".equals(method)) throw new IOException("method refused");
            URL url = new URL(request.getString("url"));
            requireCoordinatorUrl(url);
            int maximumBytes = request.getInt("maximum_bytes");
            int timeoutMs = request.getInt("timeout_ms");
            if (maximumBytes < 1 || maximumBytes > MAX_BODY_BYTES || timeoutMs < 1 || timeoutMs > 60_000) {
                throw new IOException("request bound refused");
            }
            connection = (HttpsURLConnection) url.openConnection();
            connection.setInstanceFollowRedirects(false);
            connection.setConnectTimeout(timeoutMs);
            connection.setReadTimeout(timeoutMs);
            connection.setUseCaches(false);
            connection.setRequestMethod(method);
            if (pinnedFactory != null) {
                connection.setSSLSocketFactory(pinnedFactory);
                connection.setHostnameVerifier(HttpsURLConnection.getDefaultHostnameVerifier());
            }
            applyHeaders(connection, request.getJSONObject("headers"));
            if (!request.isNull("body")) {
                byte[] body = request.getString("body").getBytes(StandardCharsets.UTF_8);
                if (body.length > 64 * 1024) throw new IOException("request body exceeds bound");
                connection.setDoOutput(true);
                connection.setFixedLengthStreamingMode(body.length);
                try (OutputStream output = connection.getOutputStream()) {
                    output.write(body);
                }
            }
            int status = connection.getResponseCode();
            long declared = connection.getContentLengthLong();
            if (declared > maximumBytes) throw new IOException("response Content-Length exceeds bound");
            InputStream source = status >= 400 ? connection.getErrorStream() : connection.getInputStream();
            byte[] body = source == null ? new byte[0] : readBounded(source, maximumBytes);
            JSONObject response = new JSONObject();
            response.put("status", status);
            response.put("body_base64", Base64.encodeToString(body, Base64.NO_WRAP));
            response.put("content_type", nullToEmpty(connection.getHeaderField("Content-Type")));
            response.put("next_challenge", nullToEmpty(connection.getHeaderField("X-Astrolabe-Next-Challenge")));
            return response.toString();
        } catch (Exception error) {
            return error(error instanceof IOException ? error.getMessage() : "native transport refused request");
        } finally {
            if (connection != null) connection.disconnect();
        }
    }

    private void deliver(String requestId, String response) {
        if (closed) return;
        String script = "globalThis.__astrolabeNativeTransportResolve("
                + JSONObject.quote(requestId) + "," + JSONObject.quote(response) + ")";
        webView.post(() -> {
            if (!closed) webView.evaluateJavascript(script, null);
        });
    }

    private static String readBootstrap(Context context) throws IOException {
        try (InputStream input = context.getAssets().open("receiver-bootstrap.json")) {
            return new String(readBounded(input, MAX_BOOTSTRAP_BYTES), StandardCharsets.UTF_8);
        }
    }

    private static void requireFields(JSONObject object, Set<String> expected) throws IOException {
        Set<String> actual = new HashSet<>();
        Iterator<String> keys = object.keys();
        while (keys.hasNext()) actual.add(keys.next());
        if (!actual.equals(expected)) throw new IOException("unknown bootstrap or request field");
    }

    private static Set<String> fields(String... values) {
        return new HashSet<>(Arrays.asList(values));
    }

    private static URL exactOrigin(String value) throws IOException {
        URL url = new URL(value);
        if (!"https".equals(url.getProtocol()) || !"".equals(url.getPath())
                || url.getQuery() != null || url.getRef() != null || url.getUserInfo() != null) {
            throw new IOException("invalid HTTPS origin");
        }
        return url;
    }

    private void requireCoordinatorUrl(URL url) throws IOException {
        int originPort = origin.getPort() == -1 ? 443 : origin.getPort();
        int requestPort = url.getPort() == -1 ? 443 : url.getPort();
        if (!"https".equals(url.getProtocol()) || !origin.getHost().equalsIgnoreCase(url.getHost())
                || originPort != requestPort || url.getUserInfo() != null || url.getRef() != null
                || url.getQuery() != null || !allowedEndpoint(url.getPath())) {
            throw new IOException("request escaped coordinator origin");
        }
    }

    private static boolean allowedEndpoint(String path) {
        if ("/head/v1/instance".equals(path) || "/head/v1/pairings".equals(path)
                || "/head/v1/pairings/status".equals(path) || "/head/v1/pairings/complete".equals(path)
                || "/head/v1/challenges".equals(path) || "/head/v1/capabilities".equals(path)
                || "/head/v1/program".equals(path) || "/head/v1/program/changes".equals(path)
                || "/head/v1/health".equals(path) || "/head/v1/live/tickets".equals(path)) {
            return true;
        }
        return path.matches("^/head/v1/assets/[0-9a-f]{64}$");
    }

    private static void applyHeaders(HttpsURLConnection connection, JSONObject headers)
            throws IOException, JSONException {
        Iterator<String> names = headers.keys();
        while (names.hasNext()) {
            String name = names.next();
            String value = headers.getString(name);
            if (!ALLOWED_HEADERS.contains(name.toLowerCase(Locale.ROOT))
                    || name.indexOf('\r') >= 0 || name.indexOf('\n') >= 0
                    || value.indexOf('\r') >= 0 || value.indexOf('\n') >= 0) {
                throw new IOException("request header refused");
            }
            connection.setRequestProperty(name, value);
        }
    }

    private static SSLSocketFactory pinnedFactory(String pem, byte[] fingerprint) throws Exception {
        String begin = "-----BEGIN CERTIFICATE-----\n";
        String end = "-----END CERTIFICATE-----\n";
        if (pem.length() < 1 || pem.length() > 16 * 1024
                || !pem.startsWith(begin) || !pem.endsWith(end)
                || pem.indexOf("-----BEGIN CERTIFICATE-----", begin.length()) >= 0
                || pem.indexOf("-----END CERTIFICATE-----") != pem.length() - end.length()) {
            throw new IOException("invalid certificate PEM");
        }
        X509Certificate certificate = (X509Certificate) CertificateFactory.getInstance("X.509")
                .generateCertificate(new ByteArrayInputStream(pem.getBytes(StandardCharsets.US_ASCII)));
        byte[] actual = MessageDigest.getInstance("SHA-256").digest(certificate.getEncoded());
        if (!MessageDigest.isEqual(actual, fingerprint)) throw new IOException("certificate fingerprint mismatch");
        X509TrustManager trust = new X509TrustManager() {
            @Override public void checkClientTrusted(X509Certificate[] chain, String authType) throws CertificateException {
                throw new CertificateException("client certificates are unsupported");
            }

            @Override public void checkServerTrusted(X509Certificate[] chain, String authType) throws CertificateException {
                if (chain == null || chain.length < 1 || authType == null || authType.isEmpty()) {
                    throw new CertificateException("missing server certificate");
                }
                try {
                    byte[] leaf = MessageDigest.getInstance("SHA-256").digest(chain[0].getEncoded());
                    if (!MessageDigest.isEqual(leaf, fingerprint)) throw new CertificateException("server certificate pin mismatch");
                } catch (CertificateException error) {
                    throw error;
                } catch (Exception error) {
                    throw new CertificateException("server certificate pin failed", error);
                }
            }

            @Override public X509Certificate[] getAcceptedIssuers() { return new X509Certificate[0]; }
        };
        SSLContext context = SSLContext.getInstance("TLS");
        context.init(null, new TrustManager[]{trust}, null);
        return context.getSocketFactory();
    }

    private static byte[] decodeFingerprint(String value) throws IOException {
        if (value.length() != 64) throw new IOException("invalid certificate fingerprint");
        byte[] result = new byte[32];
        for (int index = 0; index < result.length; index++) {
            int high = Character.digit(value.charAt(index * 2), 16);
            int low = Character.digit(value.charAt(index * 2 + 1), 16);
            if (high < 0 || low < 0 || Character.isUpperCase(value.charAt(index * 2))
                    || Character.isUpperCase(value.charAt(index * 2 + 1))) {
                throw new IOException("invalid certificate fingerprint");
            }
            result[index] = (byte) ((high << 4) | low);
        }
        return result;
    }

    private static byte[] readBounded(InputStream input, int maximumBytes) throws IOException {
        try (InputStream source = input; ByteArrayOutputStream output = new ByteArrayOutputStream()) {
            byte[] buffer = new byte[8192];
            int total = 0;
            while (true) {
                int read = source.read(buffer);
                if (read == -1) break;
                total += read;
                if (total > maximumBytes) throw new IOException("response exceeds byte bound");
                output.write(buffer, 0, read);
            }
            return output.toByteArray();
        }
    }

    private static String nullToEmpty(String value) { return value == null ? "" : value; }

    private static String error(String message) {
        JSONObject result = new JSONObject();
        try { result.put("error", message == null ? "native transport failed" : message); }
        catch (JSONException ignored) { return "{\"error\":\"native transport failed\"}"; }
        return result.toString();
    }
}
