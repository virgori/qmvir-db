# QMvir CI/CD Workflow

## 🎯 Source of Truth
**Mac Local Development** is the single source of truth. All code changes happen on Mac first.

## 🔄 Workflow

### 1. Development (Mac)
```bash
# Code on Mac
git add .
git commit -m "feature: new feature"
git push origin main
```

### 2. CI/CD Pipeline (GitLab Runners)
GitLab CI/CD automatically:
- **Test**: Runs clippy, rustfmt, tests
- **Build**: Cross-compiles for Linux x86_64
- **Deploy**: Deploys to server 161.97.146.164

### 3. Deployment (Server 161.97.146.164)
- Binary deployed to `/opt/qmvir/bin/qmvir`
- Service runs as `qmvir` systemd service
- Accessible at `161.97.146.164:5432`

## 🚀 Manual Deployment

### Option 1: GitLab CI/CD
1. Push code to GitLab
2. Go to GitLab CI/CD > Pipelines
3. Run `deploy:production` job manually

### Option 2: Direct Deploy
```bash
# On Mac - build and deploy
./scripts/deploy.sh
```

## 🔧 Configuration

### GitLab CI/CD Variables
Set these in GitLab Project Settings > CI/CD > Variables:
- `SSH_PRIVATE_KEY`: SSH key for server access

### Server Setup (161.97.146.164)
```bash
# Install dependencies
apt-get update
apt-get install -y gcc-x86_64-linux-gnu

# Service management
systemctl status qmvir
systemctl restart qmvir
journalctl -u qmvir -f
```

## 📁 File Structure
```
Mac (Source of Truth) → GitLab → CI/CD → Server (161.97.146.164)
     │                       │        │
     ▼                       ▼        ▼
  Code Changes           Build    Deploy
  git push              Test      Service
```

## ⚠️ Important Notes

1. **Never modify code directly on server** - Mac is source of truth
2. **CI/CD runs on GitLab runners** - not on local Mac
3. **Cross-compilation ensures compatibility** between Mac dev and Linux server
4. **All deployments come from repository** - no local builds on server

## 🛠 Troubleshooting

### Build Failures
```bash
# Check GitLab CI/CD logs
# Verify cross-compilation tools
# Check Rust version compatibility
```

### Deployment Issues
```bash
# On server:
systemctl status qmvir
journalctl -u qmvir -n 50
ls -la /opt/qmvir/bin/qmvir
```

### SSH Connection Issues
```bash
# Verify SSH key setup
ssh-add -l
ssh root@161.97.146.164 "echo 'Connection OK'"
```

## 📞 Support

- Mac development: Local environment
- Server issues: Check 161.97.146.164
- CI/CD issues: GitLab pipeline logs
