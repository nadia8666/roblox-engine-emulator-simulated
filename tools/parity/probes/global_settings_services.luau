-- parity: identity=4 context=edit
-- GetService of every service class rbx-dom describes, on settings() and on UserSettings(), as a plugin's code gets it (captured
-- at script identity 4, the command bar's; ree runs it in its edit run context): each root answers its own settings services,
-- refuses the other root's as its children and some other services, and answers the rest nil, making nothing.
local function Kind(Function)
	local Ok, Value = pcall(Function)
	if not Ok then
		return "error " .. P.err(Value)
	elseif Value == nil then
		return "nil"
	end
	return Value.ClassName .. ":" .. Value.Name .. "<" .. tostring(Value.Parent) .. ">"
end
local Names = {
	"AccountService", "AchievementService", "ActivityHistoryEventService", "AdService", "AnalyticsService", "AnimationClipProvider",
	"AnimationFromVideoCreatorService", "AnimationFromVideoCreatorStudioService", "AnnotationsService", "AppLifecycleObserverService", "AppRatingPromptService", "AppStorageService",
	"AppUpdateService", "AssetCounterService", "AssetDeliveryProxy", "AssetImportService", "AssetManagerService", "AssetService",
	"AudioFocusService", "AuroraScriptService", "AuroraService", "AvatarChatService", "AvatarCreationService", "AvatarEditorService",
	"AvatarImportService", "AvatarSettings", "BadgeService", "BrowserService", "BugReporterService", "BulkImportService",
	"CSGDictionaryService", "CacheableContentProvider", "CalloutService", "CaptureService", "ChangeHistoryService", "ChangeHistoryStreamingService",
	"Chat", "ChatbotUIService", "CloudCRUDService", "ClusterPacketCache", "CollaboratorsService", "CollectionService",
	"CommerceService", "ConfigService", "ConfigureServerService", "ConnectivityService", "ContentProvider", "ContextActionService",
	"ControllerService", "ConversationalAIAcceptanceService", "CookiesService", "CoreGui", "CorePackages", "CoreScriptDebuggingManagerHelper",
	"CoreScriptSyncService", "CreationDBService", "CreatorStoreService", "CrossDMScriptChangeListener", "DataModelPatchService", "DataStoreService",
	"Debris", "DebugSettings", "DebuggablePluginWatcher", "DebuggerConnectionManager", "DebuggerManager", "DebuggerUIService",
	"DeviceIdService", "DraftsService", "DraggerService", "EditableService", "EncodingService", "EventIngestService",
	"ExampleV2Service", "ExperienceAuthService", "ExperienceNotificationService", "ExperienceService", "ExperienceStateCaptureService", "ExperienceStateRecordingService",
	"ExplorerServiceVisibilityService", "FaceAnimatorService", "FacialAgeEstimationService", "FacialAnimationRecordingService", "FacialAnimationStreamingServiceV2", "FeatureRestrictionManager",
	"FlagStandService", "FlyweightService", "FriendService", "GamePassService", "GameSettings", "GamepadService",
	"GenerationService", "GenericChallengeService", "Geometry", "GeometryService", "GroupService", "GuiService",
	"GuidRegistryService", "HSRDataContentProvider", "HapticService", "HarmonyService", "HeapProfilerService", "HeatmapService",
	"HeightmapImporterService", "Hopper", "HttpRbxApiService", "HttpService", "ILegacyStudioBridge", "IXPService",
	"IncrementalPatchBuilder", "InsertService", "InstanceExtensionsService", "InternalSyncService", "JointsService", "KeyboardService",
	"KeyframeSequenceProvider", "LSPFileSyncService", "LanguageService", "LegacyStudioBridge", "Lighting", "LinkingService",
	"LiveScriptingService", "LiveSyncService", "LocalStorageService", "LocalizationService", "LodDataService", "LogReporterService",
	"LogService", "LoginService", "LuaSettings", "LuaWebService", "LuauScriptAnalyzerService", "MLModelDeliveryService",
	"MLService", "MarketplaceService", "MatchmakingService", "MaterialGenerationService", "MaterialService", "MemStorageService",
	"MemoryStoreService", "MeshContentProvider", "MessageBusService", "MessagingService", "MetaBreakpointManager", "MicroProfilerService",
	"ModerationService", "MouseService", "NetworkClient", "NetworkServer", "NetworkSettings", "NonReplicatedCSGDictionaryService",
	"NotificationService", "OmniRecommendationsService", "OpenCloudService", "PackageService", "PackageUIService", "PartyEmulatorService",
	"PatchBundlerFileWatch", "PathfindingService", "PerformanceControlService", "PermissionsService", "PhysicsService", "PhysicsSettings",
	"PlaceAssetIdsService", "PlaceStatsService", "PlacesService", "PlatformCloudStorageService", "PlatformFriendsService", "PlayerDataService",
	"PlayerEmulatorService", "PlayerHydrationService", "PlayerViewService", "Players", "PluginDebugService", "PluginGuiService",
	"PluginManagementService", "PluginPolicyService", "PointsService", "PolicyService", "ProcessInstancePhysicsService", "ProximityPromptService",
	"PublishService", "RbxAnalyticsService", "RecommendationService", "ReflectionService", "RemoteCommandService", "RemoteCursorService",
	"RemoteDebuggerServer", "RenderSettings", "ReplicatedFirst", "ReplicatedStorage", "RibbonNotificationService", "RobloxPluginGuiService",
	"RobloxReplicatedStorage", "RobloxServerStorage", "RomarkRbxAnalyticsService", "RomarkService", "RtMessagingService", "RunService",
	"RuntimeContentService", "RuntimeScriptService", "SafetyService", "ScriptChangeService", "ScriptCloneWatcher", "ScriptCloneWatcherHelper",
	"ScriptCommitService", "ScriptContext", "ScriptEditorService", "ScriptProfilerService", "ScriptRegistrationService", "ScriptService",
	"Selection", "SelectionHighlightManager", "SerializationService", "ServerScriptService", "ServerStorage", "ServiceVisibilityService",
	"SessionCheckService", "SessionService", "SharedTableRegistry", "SlimContentProvider", "SlimService", "SmoothVoxelsUpgraderService",
	"SnippetService", "SocialService", "SolidModelContentProvider", "SoundService", "SoundShimService", "SpawnerService",
	"StartPageService", "StarterGui", "StarterPack", "StarterPlayer", "StartupMessageService", "Stats",
	"StopWatchReporter", "StreamingService", "Studio", "StudioAssetService", "StudioCameraService", "StudioData",
	"StudioDeviceEmulatorService", "StudioPublishService", "StudioScriptDebugEventListener", "StudioSdkService", "StudioService", "StudioTestService",
	"StudioUserService", "StudioWidgetsService", "StylingService", "SystemThemeService", "TaskScheduler", "TeamCreateData",
	"TeamCreatePublishService", "TeamCreateService", "Teams", "TelemetryService", "TeleportService", "TemporaryCageMeshProvider",
	"TemporaryScriptService", "TestService", "TextBoxService", "TextChatService", "TextService", "TextureGenerationService",
	"ThirdPartyUserService", "TimerService", "ToastNotificationService", "TouchInputService", "TracerService", "TutorialService",
	"TweenService", "UGCAvatarService", "UGCValidationService", "UIDragDetectorService", "UniqueIdLookupService", "UnvalidatedAssetService",
	"UserGameSettings", "UserInputService", "UserService", "UserStorageService", "VRService", "VRStatusService",
	"VersionControlService", "VideoCaptureService", "VideoScreenCaptureService", "VideoService", "VirtualInputManager", "VirtualUser",
	"VisibilityCheckDispatcher", "Visit", "VisualizationModeService", "VoiceChatInternal", "VoiceChatService", "WebSocketService",
	"WebViewService", "Workspace",
}
for _, Name in Names do
	print(Name .. " " .. Kind(function()
		return settings():GetService(Name)
	end) .. " | " .. Kind(function()
		return UserSettings():GetService(Name)
	end))
end
print("children " .. #settings():GetChildren() .. " " .. #UserSettings():GetChildren())
