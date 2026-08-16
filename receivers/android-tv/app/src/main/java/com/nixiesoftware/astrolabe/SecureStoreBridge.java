package com.nixiesoftware.astrolabe;

import android.content.Context;
import android.content.SharedPreferences;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import android.webkit.JavascriptInterface;

import java.nio.charset.StandardCharsets;
import java.io.IOException;
import java.security.GeneralSecurityException;
import java.security.KeyStore;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

final class SecureStoreBridge {
    private static final String KEY_ALIAS = "astrolabe_receiver_v1";
    private static final String STORE = "astrolabe_protected_receiver";
    private static final String VALUE = "credential";
    private static final byte[] AAD = "com.nixiesoftware.astrolabe/receiver/v1"
            .getBytes(StandardCharsets.UTF_8);

    private final SharedPreferences preferences;

    SecureStoreBridge(Context context) {
        preferences = context.getSharedPreferences(STORE, Context.MODE_PRIVATE);
    }

    private SecretKey key() throws GeneralSecurityException, IOException {
        KeyStore store = KeyStore.getInstance("AndroidKeyStore");
        store.load(null);
        if (store.containsAlias(KEY_ALIAS)) {
            return (SecretKey) store.getKey(KEY_ALIAS, null);
        }
        KeyGenerator generator = KeyGenerator.getInstance(
                KeyProperties.KEY_ALGORITHM_AES,
                "AndroidKeyStore"
        );
        generator.init(new KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT
        ).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .setKeySize(256)
                .build());
        return generator.generateKey();
    }

    @JavascriptInterface
    public synchronized String load() {
        String record = preferences.getString(VALUE, null);
        if (record == null) return "";
        try {
            String[] fields = record.split(":", -1);
            if (fields.length != 3 || !"1".equals(fields[0])) {
                throw new IllegalStateException("Unsupported protected receiver state");
            }
            byte[] nonce = Base64.decode(fields[1], Base64.NO_WRAP);
            byte[] encrypted = Base64.decode(fields[2], Base64.NO_WRAP);
            if (nonce.length != 12 || encrypted.length < 16) {
                throw new IllegalStateException("Malformed protected receiver state");
            }
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.DECRYPT_MODE, key(), new GCMParameterSpec(128, nonce));
            cipher.updateAAD(AAD);
            return new String(cipher.doFinal(encrypted), StandardCharsets.UTF_8);
        } catch (GeneralSecurityException | IOException | IllegalArgumentException error) {
            throw new IllegalStateException("Receiver credential integrity check failed", error);
        }
    }

    @JavascriptInterface
    public synchronized void save(String plaintext) {
        if (plaintext == null || plaintext.length() > 16 * 1024) {
            throw new IllegalArgumentException("Receiver credential is outside its storage bound");
        }
        try {
            Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
            cipher.init(Cipher.ENCRYPT_MODE, key());
            cipher.updateAAD(AAD);
            byte[] encrypted = cipher.doFinal(plaintext.getBytes(StandardCharsets.UTF_8));
            String record = "1:"
                    + Base64.encodeToString(cipher.getIV(), Base64.NO_WRAP)
                    + ":"
                    + Base64.encodeToString(encrypted, Base64.NO_WRAP);
            if (!preferences.edit().putString(VALUE, record).commit()) {
                throw new IllegalStateException("Receiver credential commit failed");
            }
        } catch (GeneralSecurityException | IOException error) {
            throw new IllegalStateException("Receiver credential encryption failed", error);
        }
    }

    @JavascriptInterface
    public synchronized void clear() {
        if (!preferences.edit().remove(VALUE).commit()) {
            throw new IllegalStateException("Receiver credential removal failed");
        }
    }
}
