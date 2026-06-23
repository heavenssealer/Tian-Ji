# A list of the current issues 

1) The scroll bars are duplicated without reason
2) Opening a new chat should use a new memory, not the memory of the other chats so that we can start anew with different models per example
3) On MacOS, there is an issue where the system prompts the "keyring password" prompt for the app Tian-Ji, several times every x minutes, which is not optimal 
4) still on MacOS, there is an issue with the terminal when using the "compiled/built" app. The terminal suffers from a "TERM not defined" etc, and is real bad and laggy 
5) It seems like the LLM doesn't properly write nor exploit its findings; it keeps doing actions it already did. From now on, the LLM should "trace" each actions and each path it tries to take, so that it doesn't get stuck in a loops with the same ideas every time. There should be a system where the LLM saves/traces what it does and tried : example : "14:05 - 01/01/2026 : Trying to exploit CVE-2025-32423", then "15:01 - 01/01/2026 : Exploit did not make any difference, giving up on that path" etc etc. The goal is to 1 - use less token since it won't try things it already tried 2 - create tracability so that the user can try the same steps as the LLM. 
6) Token consumption still too high : let's consider adding Rust Token Killer in the loop, allowing us to spare a lot of tokens per requests. 
7) The LLM doesn't create the findings properly. Example : 
# Engagement Report — connector-htb

**Date:** 2026-06-22  
**Findings:** 29

---

## Findings

### 1. [CRITICAL] FreePBX 16.0.40.7 vulnerable to CVE-2025-57819 — Unauthenticated SQLi to RCE via /admin/ajax.php (cronjob injection)

**Target:** 10.129.105.46:80/admin/ajax.php

### 2. [CRITICAL] FreePBX 16.0.40.7 vulnerable to CVE-2025-66039 (auth bypass) + CVE-2025-61675 (SQLi→RCE) — unauthenticated RCE via custom extension injection

**Target:** 10.129.105.46:80/admin

### 3. [CRITICAL] FreePBX 16.0.40.7 vulnerable to CVE-2025-57819 (unauthenticated SQLi/RCE via /admin/ajax.php) and CVE-2025-66039 (auth bypass)

**Target:** 10.129.105.46:443/https

### 4. [CRITICAL] FreePBX admin panel accepts multiple trivial default passwords for 'admin' account: admin:password, admin:admin123, admin:Admin1234, admin:sangoma, admin:freepbx, admin:passw0rd, administrator:admin — full admin access confirmed.

**Target:** 10.129.105.46:443/https — FreePBX Admin Panel (/admin/config.php)

### 5. [CRITICAL] CVE-2025-57819: Unauthenticated SQLi to RCE confirmed — webshell deployed at https://10.129.105.46/this-is-an-ioc-not-actually-watchTowr-dw415l5tsq.php?cmd=hostname via watchtowrlabs PoC; no credentials required

**Target:** 10.129.105.46:443/https — FreePBX 16.0.40.7 /admin/ajax.php

### 6. [HIGH] FreePBX Administration panel version 16.0.40.7 exposed on HTTP and HTTPS — known to have multiple CVEs including RCE

**Target:** 10.129.105.46:80/admin

### 7. [HIGH] FreePBX 16.0.40.7 is below patched versions 16.0.44+ (CVE-2025-66039 auth bypass) and 16.0.66+ (CVE-2025-57819 unauthenticated SQLi/RCE via /admin/ajax.php)

**Target:** 10.129.105.46:443/https

### 8. [HIGH] FreePBX 16.0.40.7 admin panel exposed on HTTPS — known vulnerable PBX version with multiple CVEs

**Target:** 10.129.105.46:443/https

### 9. [HIGH] CVE-2025-57819: Unauthenticated SQLi/RCE via /admin/ajax.php in FreePBX <16.0.66 — critical unpatched vulnerability on this host

**Target:** 10.129.105.46:443/tcp (FreePBX 16.0.40.7)

### 10. [HIGH] CVE-2025-66039: Authentication bypass in FreePBX <16.0.44 — admin panel access without valid credentials

**Target:** 10.129.105.46:443/tcp (FreePBX 16.0.40.7)

### 11. [MEDIUM] Asterisk Manager Interface (AMI) port 5038 is filtered — firewall-protected but service likely running behind it; credential brute-force risk if bypassed

**Target:** 10.129.105.46:5038/tcp

### 12. [MEDIUM] SIP port 5060 TCP filtered, UDP open|filtered — Asterisk SIP service likely running; susceptible to SIP enumeration/toll fraud if reachable

**Target:** 10.129.105.46:5060/tcp+udp

### 13. [MEDIUM] FreePBX 16.0.40.7 admin panel publicly exposed on HTTPS; version string leaked in all asset URLs via load_version parameter

**Target:** 10.129.105.46:443/https

### 14. [MEDIUM] FreePBX admin panel /admin/config.php serves HTTP 200 with full application HTML (CSS/JS assets, version string 16.0.40.7, login form) to unauthenticated requests — no pre-auth redirect or access control enforced at the HTTP layer

**Target:** 10.129.105.46:443/admin/config.php

### 15. [LOW] MySQL port 3306 filtered — FreePBX backend database running locally; not directly accessible but relevant for post-exploitation pivot

**Target:** 10.129.105.46:3306/tcp

### 16. [INFO] SSH service running OpenSSH 7.4 on CentOS

**Target:** 10.129.105.46:22/tcp

### 17. [INFO] HTTP redirects to http://connected.htb/ — Apache 2.4.6 on CentOS with PHP 7.4.16

**Target:** 10.129.105.46:80/tcp

### 18. [INFO] HTTPS service with SSL cert CN=pbxconnect — PBX application, robots.txt disallows /, Apache 2.4.6/PHP 7.4.16

**Target:** 10.129.105.46:443/tcp

### 19. [INFO] OpenSSH 7.4 open — older version, potential CVEs but no anonymous access

**Target:** 10.129.105.46:22/ssh

### 20. [INFO] Apache 2.4.6 / PHP 7.4.16 on CentOS — default request redirects to config.php (404); robots.txt disallows /

**Target:** 10.129.105.46:80/http

### 21. [INFO] Apache 2.4.6 / PHP 7.4.16 with self-signed cert for CN=pbxconnect — suggests a PBX web application

**Target:** 10.129.105.46:443/https

### 22. [INFO] FreePBX 16.0.40.7 administration panel exposed on HTTPS at /admin/config.php (connected.htb)

**Target:** 10.129.105.46:443/https

### 23. [INFO] OpenSSH 7.4 (protocol 2.0) open — RSA/ECDSA/ED25519 host keys exposed; version is EOL and lacks patches post-2018

**Target:** 10.129.105.46:22/tcp

### 24. [INFO] Apache 2.4.6 / PHP 7.4.16 / OpenSSL 1.0.2k-fips on HTTP — robots.txt disallows /; 404 on config.php (likely redirects to HTTPS)

**Target:** 10.129.105.46:80/tcp

### 25. [INFO] FreePBX 16.0.40.7 admin panel on HTTPS; self-signed cert CN=pbxconnect; Apache 2.4.6 / PHP 7.4.16 / OpenSSL 1.0.2k-fips

**Target:** 10.129.105.46:443/tcp

### 26. [INFO] OpenSSH 7.4 open on port 22

**Target:** 10.129.105.46:22/ssh

### 27. [INFO] OpenSSH 7.4 (protocol 2.0) open — CentOS host, older SSH version with known informational disclosures

**Target:** 10.129.105.109:22/ssh

### 28. [INFO] Apache 2.4.6 (CentOS) with OpenSSL/1.0.2k-fips and PHP/7.4.16 — FreePBX HTTP service

**Target:** 10.129.105.109:80/http

### 29. [INFO] Apache 2.4.6 (CentOS) with OpenSSL/1.0.2k-fips and PHP/7.4.16 — FreePBX HTTPS admin panel

**Target:** 10.129.105.109:443/https


This is not a correct "Findings" management, it doesn't record the important stuff, like the user flag that we found, or the moment we succeeded in getting the reverse shell. 



