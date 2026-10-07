# 文档索引

本目录保留当前产品、实现与测试约定。源码及自动化测试描述运行行为，平台集成还需在实际 App 中验收。

## 一、契约

产品范围见 [PRODUCT.md](../PRODUCT.md)，界面规范见 [DESIGN.md](../DESIGN.md)，共享 Core 与桌面、CLI 的接口见 [app-contract.md](app-contract.md)。引导选品、上架事件和突出提醒的业务边界见 [onboarding-alert-core.md](onboarding-alert-core.md)。

## 二、实现

三层职责、生命周期和状态流转见 [architecture.md](architecture.md)。理光接口的请求签名和响应字段见 [product-api-signature.md](product-api-signature.md)，列表监控的分页和库存语义见 [list-monitoring-workplan.md](list-monitoring-workplan.md)，商品资料与图片的来源约定见 [product-image-workplan.md](product-image-workplan.md)。

## 三、验证

产品行为和测试方法分别见 [specification.md](specification.md) 与 [testing.md](testing.md)。测试命令与桌面启动入口见 [项目说明](../README.md)。

正式发布前逐项检查 [macOS 发版验收清单](release-checklist.md)。自动更新与发布流程见 [updates.md](updates.md)。

开发时先明确验收场景，再添加失败测试并实现目标行为。自动化测试与原生平台验收分别判断，权限、桌面图层和真实投递以实际观察为准。
